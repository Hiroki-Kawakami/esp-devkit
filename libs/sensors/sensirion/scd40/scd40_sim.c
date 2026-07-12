/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "scd40_sim.h"
#include "sensirion_i2c.h"
#include "simulator/i2c_master_sim.h"
#include "esp_timer.h"
#include <stdlib.h>
#include <string.h>

#define SCD40_SIM_PERIOD_US            (5 * 1000 * 1000)
#define SCD40_SIM_LOW_POWER_PERIOD_US  (30 * 1000 * 1000)

typedef enum {
    SCD40_SIM_IDLE,
    SCD40_SIM_MEASURING,
    SCD40_SIM_LOW_POWER,
} scd40_sim_mode_t;

struct scd40_sim {
    i2c_master_bus_handle_t bus;
    uint16_t                address;
    scd40_sim_mode_t        mode;
    int64_t                 last_sample_us;
    bool                    env_override;
    scd40_data_t            env;
    scd40_data_t            walk;
    uint32_t                lcg;
    bool                    asc_enabled;
    uint16_t                temp_offset_raw;
    uint16_t                altitude_m;
    uint16_t                pressure_raw;
    uint8_t                 resp[SENSIRION_MAX_WORDS * 3];
    size_t                  resp_len;
};

static float walk_step(scd40_sim_t *chip, float value, float step, float min, float max) {
    chip->lcg = chip->lcg * 1103515245u + 12345u;
    value += ((float)((chip->lcg >> 16) % 201) - 100.0f) / 100.0f * step;
    return value < min ? min : (value > max ? max : value);
}

static void walk_advance(scd40_sim_t *chip) {
    chip->walk.co2_ppm     = (uint16_t)walk_step(chip, (float)chip->walk.co2_ppm, 20.0f, 420.0f, 2500.0f);
    chip->walk.humidity    = walk_step(chip, chip->walk.humidity, 0.5f, 20.0f, 80.0f);
    chip->walk.temperature = walk_step(chip, chip->walk.temperature, 0.1f, 15.0f, 35.0f);
}

static void walk_reset(scd40_sim_t *chip) {
    chip->walk = (scd40_data_t){ .co2_ppm = 600, .temperature = 25.0f, .humidity = 50.0f };
}

static int64_t period_us(const scd40_sim_t *chip) {
    return chip->mode == SCD40_SIM_LOW_POWER ? SCD40_SIM_LOW_POWER_PERIOD_US : SCD40_SIM_PERIOD_US;
}

static bool is_ready(const scd40_sim_t *chip, int64_t now) {
    return chip->mode != SCD40_SIM_IDLE && now - chip->last_sample_us >= period_us(chip);
}

static void stage_words(scd40_sim_t *chip, const uint16_t *words, size_t count) {
    sensirion_pack_words(words, count, chip->resp);
    chip->resp_len = count * 3;
}

static esp_err_t start_mode(scd40_sim_t *chip, scd40_sim_mode_t mode, int64_t now) {
    if (chip->mode != SCD40_SIM_IDLE) return ESP_FAIL;
    chip->mode = mode;
    chip->last_sample_us = now;
    return ESP_OK;
}

static esp_err_t scd40_sim_on_write(void *ctx, const uint8_t *data, size_t len) {
    scd40_sim_t *chip = ctx;
    if (len < 2) return ESP_FAIL;
    uint16_t cmd = (uint16_t)((data[0] << 8) | data[1]);
    uint16_t arg = 0;
    if (len == 5) {
        if (sensirion_unpack_words(&data[2], 3, &arg, 1) != ESP_OK) return ESP_FAIL;
    } else if (len != 2) {
        return ESP_FAIL;
    }
    int64_t now = esp_timer_get_time();
    chip->resp_len = 0;
    bool idle = chip->mode == SCD40_SIM_IDLE;

    switch (cmd) {
        case 0x21B1:  /* start periodic measurement */
            return start_mode(chip, SCD40_SIM_MEASURING, now);
        case 0x21AC:  /* start low-power periodic measurement */
            return start_mode(chip, SCD40_SIM_LOW_POWER, now);
        case 0x3F86:  /* stop periodic measurement */
            chip->mode = SCD40_SIM_IDLE;
            return ESP_OK;
        case 0xE4B8: {  /* get data-ready status */
            const uint16_t word = is_ready(chip, now) ? 1 : 0;
            stage_words(chip, &word, 1);
            return ESP_OK;
        }
        case 0xEC05: {  /* read measurement */
            if (!is_ready(chip, now)) return ESP_FAIL;
            if (!chip->env_override) walk_advance(chip);
            chip->last_sample_us = now;
            const scd40_data_t v = chip->env_override ? chip->env : chip->walk;
            const uint16_t words[3] = {
                v.co2_ppm,
                (uint16_t)((v.temperature + 45.0f) * 65535.0f / 175.0f + 0.5f),
                (uint16_t)(v.humidity * 65535.0f / 100.0f + 0.5f),
            };
            stage_words(chip, words, 3);
            return ESP_OK;
        }
        case 0x2416:  /* set ASC enabled */
            if (!idle) return ESP_FAIL;
            chip->asc_enabled = arg != 0;
            return ESP_OK;
        case 0x2313: {  /* get ASC enabled */
            if (!idle) return ESP_FAIL;
            const uint16_t word = chip->asc_enabled ? 1 : 0;
            stage_words(chip, &word, 1);
            return ESP_OK;
        }
        case 0x362F: {  /* forced recalibration: report correction 0 */
            if (!idle) return ESP_FAIL;
            const uint16_t word = 0x8000;
            stage_words(chip, &word, 1);
            return ESP_OK;
        }
        case 0x241D:  /* set temperature offset */
            if (!idle) return ESP_FAIL;
            chip->temp_offset_raw = arg;
            return ESP_OK;
        case 0x2427:  /* set sensor altitude */
            if (!idle) return ESP_FAIL;
            chip->altitude_m = arg;
            return ESP_OK;
        case 0xE000:  /* set ambient pressure (allowed while measuring) */
            chip->pressure_raw = arg;
            return ESP_OK;
        case 0x3615:  /* persist settings */
            return idle ? ESP_OK : ESP_FAIL;
        case 0x3682: {  /* read serial number */
            if (!idle) return ESP_FAIL;
            const uint16_t words[3] = { 0x5CD4, 0x0051, 0x0001 };
            stage_words(chip, words, 3);
            return ESP_OK;
        }
        case 0x3646:  /* reinit */
            if (!idle) return ESP_FAIL;
            walk_reset(chip);
            return ESP_OK;
        default:
            return ESP_FAIL;
    }
}

static esp_err_t scd40_sim_on_read(void *ctx, uint8_t *data, size_t len) {
    scd40_sim_t *chip = ctx;
    if (len == 0 || len > chip->resp_len) return ESP_FAIL;
    memcpy(data, chip->resp, len);
    chip->resp_len = 0;
    return ESP_OK;
}

esp_err_t scd40_sim_attach(i2c_master_bus_handle_t bus, uint8_t address, scd40_sim_t **out_chip) {
    if (!bus) return ESP_ERR_INVALID_ARG;
    scd40_sim_t *chip = calloc(1, sizeof(*chip));
    if (!chip) return ESP_ERR_NO_MEM;
    chip->bus = bus;
    chip->address = address ? address : SCD40_I2C_ADDR;
    chip->lcg = 0x5CD40u;
    chip->asc_enabled = true;
    walk_reset(chip);

    const i2c_sim_device_ops_t ops = {
        .on_write = scd40_sim_on_write,
        .on_read  = scd40_sim_on_read,
    };
    esp_err_t err = i2c_sim_attach(bus, chip->address, &ops, chip);
    if (err != ESP_OK) {
        free(chip);
        return err;
    }
    if (out_chip) *out_chip = chip;
    return ESP_OK;
}

void scd40_sim_detach(scd40_sim_t *chip) {
    if (!chip) return;
    i2c_sim_detach(chip->bus, chip->address);
    free(chip);
}

void scd40_sim_set_environment(scd40_sim_t *chip, const scd40_data_t *data) {
    if (!chip) return;
    chip->env_override = data != NULL;
    if (data) chip->env = *data;
}
