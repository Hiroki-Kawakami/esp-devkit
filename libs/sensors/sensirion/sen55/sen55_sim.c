/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Warm-up is shortened for sim usefulness: VOC index appears 2 s after start,
 * NOx after 10 s (the real chip takes longer for NOx).
 */

#include "sen55_sim.h"
#include "sensirion_i2c.h"
#include "simulator/i2c_master_sim.h"
#include "esp_timer.h"
#include <math.h>
#include <stdlib.h>
#include <string.h>

#define SEN55_SIM_PERIOD_US       (1000 * 1000)
#define SEN55_SIM_VOC_WARMUP_US   (2 * 1000 * 1000)
#define SEN55_SIM_NOX_WARMUP_US   (10 * 1000 * 1000)

typedef enum {
    SEN55_SIM_IDLE,
    SEN55_SIM_MEASURING,
    SEN55_SIM_RHT_GAS_ONLY,
} sen55_sim_mode_t;

struct sen55_sim {
    i2c_master_bus_handle_t bus;
    uint16_t                address;
    sen55_sim_mode_t        mode;
    int64_t                 mode_start_us;
    int64_t                 last_sample_us;
    bool                    have_sample;
    bool                    env_override;
    sen55_data_t            env;
    sen55_data_t            walk;
    uint32_t                lcg;
    uint8_t                 resp[SENSIRION_MAX_WORDS * 3];
    size_t                  resp_len;
};

static float walk_step(sen55_sim_t *chip, float value, float step, float min, float max) {
    chip->lcg = chip->lcg * 1103515245u + 12345u;
    value += ((float)((chip->lcg >> 16) % 201) - 100.0f) / 100.0f * step;
    return value < min ? min : (value > max ? max : value);
}

static void walk_advance(sen55_sim_t *chip) {
    chip->walk.pm2_5       = walk_step(chip, chip->walk.pm2_5, 0.4f, 2.0f, 60.0f);
    chip->walk.pm1_0       = chip->walk.pm2_5 * 0.7f;
    chip->walk.pm4_0       = chip->walk.pm2_5 * 1.15f;
    chip->walk.pm10_0      = chip->walk.pm2_5 * 1.3f;
    chip->walk.humidity    = walk_step(chip, chip->walk.humidity, 0.5f, 20.0f, 80.0f);
    chip->walk.temperature = walk_step(chip, chip->walk.temperature, 0.1f, 15.0f, 35.0f);
    chip->walk.voc_index   = walk_step(chip, chip->walk.voc_index, 2.0f, 1.0f, 500.0f);
    chip->walk.nox_index   = walk_step(chip, chip->walk.nox_index, 1.0f, 1.0f, 500.0f);
}

static void walk_reset(sen55_sim_t *chip) {
    chip->walk = (sen55_data_t){
        .pm1_0 = 5.6f, .pm2_5 = 8.0f, .pm4_0 = 9.2f, .pm10_0 = 10.4f,
        .humidity = 50.0f, .temperature = 25.0f,
        .voc_index = 100.0f, .nox_index = 20.0f,
    };
}

static bool is_ready(const sen55_sim_t *chip, int64_t now) {
    return chip->mode != SEN55_SIM_IDLE && now - chip->last_sample_us >= SEN55_SIM_PERIOD_US;
}

static uint16_t enc_uint(float value, float mul) {
    return isnan(value) ? 0xFFFF : (uint16_t)(value * mul + 0.5f);
}

static uint16_t enc_int(float value, float mul) {
    return isnan(value) ? 0x7FFF : (uint16_t)(int16_t)(value * mul + (value >= 0 ? 0.5f : -0.5f));
}

static void stage_words(sen55_sim_t *chip, const uint16_t *words, size_t count) {
    sensirion_pack_words(words, count, chip->resp);
    chip->resp_len = count * 3;
}

static void stage_ascii(sen55_sim_t *chip, const char *text) {
    uint8_t raw[32] = { 0 };
    strncpy((char *)raw, text, sizeof(raw));
    sensirion_pack_bytes(raw, sizeof(raw), chip->resp);
    chip->resp_len = sizeof(raw) / 2 * 3;
}

static void stage_measured_values(sen55_sim_t *chip, int64_t now) {
    if (is_ready(chip, now)) {
        if (!chip->env_override) walk_advance(chip);
        chip->last_sample_us = now;
        chip->have_sample = true;
    }
    sen55_data_t v = chip->env_override ? chip->env : chip->walk;
    if (!chip->have_sample) {
        v = (sen55_data_t){ NAN, NAN, NAN, NAN, NAN, NAN, NAN, NAN };
    } else {
        if (chip->mode == SEN55_SIM_RHT_GAS_ONLY) {
            v.pm1_0 = v.pm2_5 = v.pm4_0 = v.pm10_0 = NAN;
        }
        if (now - chip->mode_start_us < SEN55_SIM_VOC_WARMUP_US) v.voc_index = NAN;
        if (now - chip->mode_start_us < SEN55_SIM_NOX_WARMUP_US) v.nox_index = NAN;
    }
    const uint16_t words[8] = {
        enc_uint(v.pm1_0, 10.0f), enc_uint(v.pm2_5, 10.0f),
        enc_uint(v.pm4_0, 10.0f), enc_uint(v.pm10_0, 10.0f),
        enc_int(v.humidity, 100.0f), enc_int(v.temperature, 200.0f),
        enc_int(v.voc_index, 10.0f), enc_int(v.nox_index, 10.0f),
    };
    stage_words(chip, words, 8);
}

static esp_err_t start_mode(sen55_sim_t *chip, sen55_sim_mode_t mode, int64_t now) {
    if (chip->mode != SEN55_SIM_IDLE) return ESP_FAIL;
    chip->mode = mode;
    chip->mode_start_us = now;
    chip->last_sample_us = now;
    chip->have_sample = false;
    return ESP_OK;
}

static esp_err_t sen55_sim_on_write(void *ctx, const uint8_t *data, size_t len) {
    sen55_sim_t *chip = ctx;
    if (len < 2) return ESP_FAIL;
    uint16_t cmd = (uint16_t)((data[0] << 8) | data[1]);
    if (len != 2) return ESP_FAIL;  /* no payload-carrying commands emulated */
    int64_t now = esp_timer_get_time();
    chip->resp_len = 0;

    switch (cmd) {
        case 0x0021:  /* start measurement */
            return start_mode(chip, SEN55_SIM_MEASURING, now);
        case 0x0037:  /* start RHT/gas-only measurement */
            return start_mode(chip, SEN55_SIM_RHT_GAS_ONLY, now);
        case 0x0104:  /* stop measurement */
            chip->mode = SEN55_SIM_IDLE;
            return ESP_OK;
        case 0x0202: {  /* read data-ready flag */
            const uint16_t word = is_ready(chip, now) ? 1 : 0;
            stage_words(chip, &word, 1);
            return ESP_OK;
        }
        case 0x03C4:  /* read measured values */
            if (chip->mode == SEN55_SIM_IDLE) return ESP_FAIL;
            stage_measured_values(chip, now);
            return ESP_OK;
        case 0x5607:  /* start fan cleaning */
            return chip->mode == SEN55_SIM_MEASURING ? ESP_OK : ESP_FAIL;
        case 0xD014:
            stage_ascii(chip, "SEN55");
            return ESP_OK;
        case 0xD033:
            stage_ascii(chip, "SEN55-SIM-000001");
            return ESP_OK;
        case 0xD206:
        case 0xD210: {  /* device status (read / read-and-clear): no faults */
            const uint16_t words[2] = { 0, 0 };
            stage_words(chip, words, 2);
            return ESP_OK;
        }
        case 0xD304:  /* reset */
            chip->mode = SEN55_SIM_IDLE;
            walk_reset(chip);
            return ESP_OK;
        default:
            return ESP_FAIL;
    }
}

static esp_err_t sen55_sim_on_read(void *ctx, uint8_t *data, size_t len) {
    sen55_sim_t *chip = ctx;
    if (len == 0 || len > chip->resp_len) return ESP_FAIL;
    memcpy(data, chip->resp, len);
    chip->resp_len = 0;
    return ESP_OK;
}

esp_err_t sen55_sim_attach(i2c_master_bus_handle_t bus, uint8_t address, sen55_sim_t **out_chip) {
    if (!bus) return ESP_ERR_INVALID_ARG;
    sen55_sim_t *chip = calloc(1, sizeof(*chip));
    if (!chip) return ESP_ERR_NO_MEM;
    chip->bus = bus;
    chip->address = address ? address : SEN55_I2C_ADDR;
    chip->lcg = 0x5EA55u;
    walk_reset(chip);

    const i2c_sim_device_ops_t ops = {
        .on_write = sen55_sim_on_write,
        .on_read  = sen55_sim_on_read,
    };
    esp_err_t err = i2c_sim_attach(bus, chip->address, &ops, chip);
    if (err != ESP_OK) {
        free(chip);
        return err;
    }
    if (out_chip) *out_chip = chip;
    return ESP_OK;
}

void sen55_sim_detach(sen55_sim_t *chip) {
    if (!chip) return;
    i2c_sim_detach(chip->bus, chip->address);
    free(chip);
}

void sen55_sim_set_environment(sen55_sim_t *chip, const sen55_data_t *data) {
    if (!chip) return;
    chip->env_override = data != NULL;
    if (data) chip->env = *data;
}
