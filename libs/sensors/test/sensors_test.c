/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host unit test for libs/sensors: the real SEN55/SCD40 drivers against their
 * chip emulators over the idf_compat virtual I2C bus. Covers the Sensirion
 * framing/CRC, create-time probing (present/absent), the mode state machines
 * with their idle-only command rules, the data-ready cadence (read clears it),
 * warm-up NANs, and value round-trips through the wire encoding via
 * *_sim_set_environment. Sleeps through real 1 s / 5 s measurement periods, so
 * the run takes ~10 s.
 */

#include "driver/i2c_master.h"
#include "simulator/i2c_master_sim.h"
#include "sensirion_i2c.h"
#include "sen55.h"
#include "sen55_sim.h"
#include "scd40.h"
#include "scd40_sim.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(cond) do { \
    if (!(cond)) { \
        fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
        exit(1); \
    } \
} while (0)

static void msleep(uint32_t ms) { vTaskDelay(pdMS_TO_TICKS(ms)); }

static void test_framing(void) {
    const uint8_t beef[2] = { 0xBE, 0xEF };
    CHECK(sensirion_crc8(beef, 2) == 0x92);

    const uint16_t words[3] = { 0x1234, 0xBEEF, 0x0000 };
    uint8_t wire[9];
    uint16_t out[3];
    sensirion_pack_words(words, 3, wire);
    CHECK(sensirion_unpack_words(wire, sizeof(wire), out, 3) == ESP_OK);
    CHECK(memcmp(words, out, sizeof(words)) == 0);
    wire[2] ^= 0xFF;
    CHECK(sensirion_unpack_words(wire, sizeof(wire), out, 3) == ESP_ERR_INVALID_CRC);
    printf("framing ok\n");
}

static void test_sen55(i2c_master_bus_handle_t bus, sen55_sim_t *chip) {
    sen55_t *sen = NULL;
    const sen55_config_t cfg = { .i2c_bus = bus };
    CHECK(sen55_create(&cfg, &sen) == ESP_OK);

    char serial[33];
    CHECK(sen55_read_serial(sen, serial, sizeof(serial)) == ESP_OK);
    CHECK(strcmp(serial, "SEN55-SIM-000001") == 0);

    uint32_t status = 1;
    CHECK(sen55_read_status(sen, &status) == ESP_OK && status == 0);

    sen55_data_t air;
    CHECK(sen55_read(sen, &air) != ESP_OK);       /* idle: chip NACKs */
    CHECK(sen55_fan_clean(sen) != ESP_OK);        /* idle: chip NACKs */

    CHECK(sen55_start(sen) == ESP_OK);
    CHECK(sen55_start(sen) != ESP_OK);            /* already measuring */
    bool ready = true;
    CHECK(sen55_data_ready(sen, &ready) == ESP_OK && !ready);
    msleep(1100);
    CHECK(sen55_data_ready(sen, &ready) == ESP_OK && ready);
    CHECK(sen55_read(sen, &air) == ESP_OK);
    CHECK(air.pm2_5 > 0.0f && air.pm2_5 < 100.0f);
    CHECK(air.temperature > 0.0f && air.temperature < 40.0f);
    CHECK(isnan(air.voc_index) && isnan(air.nox_index));  /* warm-up */
    CHECK(sen55_data_ready(sen, &ready) == ESP_OK && !ready);  /* read cleared it */

    const sen55_data_t env = {
        .pm1_0 = 1.0f, .pm2_5 = 2.0f, .pm4_0 = 3.0f, .pm10_0 = 4.0f,
        .humidity = 40.0f, .temperature = 21.5f,
        .voc_index = 123.0f, .nox_index = 45.0f,
    };
    sen55_sim_set_environment(chip, &env);
    msleep(1100);
    CHECK(sen55_read(sen, &air) == ESP_OK);
    CHECK(fabsf(air.pm2_5 - 2.0f) < 0.1f);
    CHECK(fabsf(air.humidity - 40.0f) < 0.01f);
    CHECK(fabsf(air.temperature - 21.5f) < 0.01f);
    CHECK(fabsf(air.voc_index - 123.0f) < 0.1f);  /* past the 2 s VOC warm-up */
    CHECK(isnan(air.nox_index));                  /* still inside the 10 s NOx warm-up */
    sen55_sim_set_environment(chip, NULL);

    CHECK(sen55_stop(sen) == ESP_OK);
    CHECK(sen55_fan_clean(sen) != ESP_OK);
    sen55_delete(sen);
    printf("sen55 ok\n");
}

static void test_scd40(i2c_master_bus_handle_t bus, scd40_sim_t *chip) {
    scd40_t *co2 = NULL;
    const scd40_config_t cfg = { .i2c_bus = bus };
    CHECK(scd40_create(&cfg, &co2) == ESP_OK);

    uint64_t serial = 0;
    CHECK(scd40_read_serial(co2, &serial) == ESP_OK && serial == 0x5CD400510001ULL);

    bool asc = false;
    CHECK(scd40_get_asc_enabled(co2, &asc) == ESP_OK && asc);
    CHECK(scd40_set_asc_enabled(co2, false) == ESP_OK);
    CHECK(scd40_get_asc_enabled(co2, &asc) == ESP_OK && !asc);

    int16_t correction = 99;
    CHECK(scd40_forced_recalibration(co2, 600, &correction) == ESP_OK && correction == 0);
    CHECK(scd40_set_temperature_offset(co2, 4.0f) == ESP_OK);
    CHECK(scd40_set_altitude(co2, 100) == ESP_OK);

    CHECK(scd40_stop(co2) == ESP_OK);             /* idle stop is a no-op */
    CHECK(scd40_start(co2) == ESP_OK);
    CHECK(scd40_start(co2) != ESP_OK);            /* already measuring */
    CHECK(scd40_set_asc_enabled(co2, true) != ESP_OK);  /* idle-only */
    CHECK(scd40_set_ambient_pressure(co2, 101300) == ESP_OK);  /* allowed while measuring */

    scd40_data_t m;
    bool ready = true;
    CHECK(scd40_data_ready(co2, &ready) == ESP_OK && !ready);
    CHECK(scd40_read(co2, &m) != ESP_OK);         /* not ready: chip NACKs */

    const scd40_data_t env = { .co2_ppm = 800, .temperature = 22.0f, .humidity = 45.0f };
    scd40_sim_set_environment(chip, &env);
    msleep(5100);
    CHECK(scd40_data_ready(co2, &ready) == ESP_OK && ready);
    CHECK(scd40_read(co2, &m) == ESP_OK);
    CHECK(m.co2_ppm == 800);
    CHECK(fabsf(m.temperature - 22.0f) < 0.01f);
    CHECK(fabsf(m.humidity - 45.0f) < 0.01f);
    CHECK(scd40_data_ready(co2, &ready) == ESP_OK && !ready);  /* read cleared it */

    CHECK(scd40_stop(co2) == ESP_OK);
    scd40_delete(co2);
    printf("scd40 ok\n");
}

int main(void) {
    test_framing();

    const i2c_master_bus_config_t bus_cfg = { .i2c_port = 0, .sda_io_num = -1, .scl_io_num = -1 };
    i2c_master_bus_handle_t bus = NULL;
    CHECK(i2c_new_master_bus(&bus_cfg, &bus) == ESP_OK);

    sen55_t *sen = NULL;
    const sen55_config_t sen_cfg = { .i2c_bus = bus };
    CHECK(sen55_create(&sen_cfg, &sen) == ESP_ERR_NOT_FOUND);  /* nothing attached yet */

    sen55_sim_t *sen_chip = NULL;
    scd40_sim_t *co2_chip = NULL;
    CHECK(sen55_sim_attach(bus, 0, &sen_chip) == ESP_OK);
    CHECK(scd40_sim_attach(bus, 0, &co2_chip) == ESP_OK);

    test_sen55(bus, sen_chip);
    test_scd40(bus, co2_chip);

    scd40_sim_detach(co2_chip);
    scd40_t *co2 = NULL;
    const scd40_config_t co2_cfg = { .i2c_bus = bus };
    CHECK(scd40_create(&co2_cfg, &co2) == ESP_ERR_NOT_FOUND);  /* detached chip is gone */

    printf("sensors_test: all tests passed\n");
    return 0;
}
