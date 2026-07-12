/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sensirion_i2c.h"
#include "crc8.h"

uint8_t sensirion_crc8(const uint8_t *data, size_t len) {
    return sensor_crc8(0x31, 0xFF, data, len);
}

void sensirion_pack_words(const uint16_t *words, size_t count, uint8_t *out) {
    for (size_t i = 0; i < count; i++) {
        out[i * 3 + 0] = (uint8_t)(words[i] >> 8);
        out[i * 3 + 1] = (uint8_t)(words[i] & 0xFF);
        out[i * 3 + 2] = sensirion_crc8(&out[i * 3], 2);
    }
}

void sensirion_pack_bytes(const uint8_t *data, size_t count, uint8_t *out) {
    for (size_t i = 0; i < count / 2; i++) {
        out[i * 3 + 0] = data[i * 2];
        out[i * 3 + 1] = data[i * 2 + 1];
        out[i * 3 + 2] = sensirion_crc8(&out[i * 3], 2);
    }
}

esp_err_t sensirion_unpack_words(const uint8_t *in, size_t in_len, uint16_t *words, size_t count) {
    if (in_len != count * 3) return ESP_ERR_INVALID_SIZE;
    for (size_t i = 0; i < count; i++) {
        const uint8_t *p = &in[i * 3];
        if (sensirion_crc8(p, 2) != p[2]) return ESP_ERR_INVALID_CRC;
        words[i] = (uint16_t)((p[0] << 8) | p[1]);
    }
    return ESP_OK;
}

esp_err_t sensirion_write_cmd(i2c_master_dev_handle_t dev, uint16_t cmd) {
    uint8_t buf[2] = { (uint8_t)(cmd >> 8), (uint8_t)(cmd & 0xFF) };
    return sensor_i2c_write(dev, buf, sizeof(buf));
}

esp_err_t sensirion_write_cmd_words(i2c_master_dev_handle_t dev, uint16_t cmd,
                                    const uint16_t *words, size_t count) {
    if (count > SENSIRION_MAX_WORDS) return ESP_ERR_INVALID_ARG;
    uint8_t buf[2 + SENSIRION_MAX_WORDS * 3];
    buf[0] = (uint8_t)(cmd >> 8);
    buf[1] = (uint8_t)(cmd & 0xFF);
    sensirion_pack_words(words, count, &buf[2]);
    return sensor_i2c_write(dev, buf, 2 + count * 3);
}

esp_err_t sensirion_read_words(i2c_master_dev_handle_t dev, uint16_t *words, size_t count) {
    if (count > SENSIRION_MAX_WORDS) return ESP_ERR_INVALID_ARG;
    uint8_t rx[SENSIRION_MAX_WORDS * 3];
    esp_err_t err = sensor_i2c_read(dev, rx, count * 3);
    if (err != ESP_OK) return err;
    return sensirion_unpack_words(rx, count * 3, words, count);
}

esp_err_t sensirion_read_bytes(i2c_master_dev_handle_t dev, uint8_t *data, size_t count) {
    if (count / 2 > SENSIRION_MAX_WORDS) return ESP_ERR_INVALID_ARG;
    uint8_t rx[SENSIRION_MAX_WORDS * 3];
    size_t n_pairs = count / 2;
    esp_err_t err = sensor_i2c_read(dev, rx, n_pairs * 3);
    if (err != ESP_OK) return err;
    for (size_t i = 0; i < n_pairs; i++) {
        const uint8_t *p = &rx[i * 3];
        if (sensirion_crc8(p, 2) != p[2]) return ESP_ERR_INVALID_CRC;
        data[i * 2]     = p[0];
        data[i * 2 + 1] = p[1];
    }
    return ESP_OK;
}

esp_err_t sensirion_cmd_read(i2c_master_dev_handle_t dev, uint16_t cmd, uint32_t delay_ms,
                             uint16_t *words, size_t count) {
    if (count > SENSIRION_MAX_WORDS) return ESP_ERR_INVALID_ARG;
    uint8_t w[2] = { (uint8_t)(cmd >> 8), (uint8_t)(cmd & 0xFF) };
    uint8_t rx[SENSIRION_MAX_WORDS * 3];
    esp_err_t err = sensor_i2c_write_delay_read(dev, w, sizeof(w), delay_ms, rx, count * 3);
    if (err != ESP_OK) return err;
    return sensirion_unpack_words(rx, count * 3, words, count);
}

esp_err_t sensirion_cmd_write_read(i2c_master_dev_handle_t dev, uint16_t cmd,
                                   const uint16_t *wwords, size_t wcount, uint32_t delay_ms,
                                   uint16_t *rwords, size_t rcount) {
    if (wcount > SENSIRION_MAX_WORDS || rcount > SENSIRION_MAX_WORDS) return ESP_ERR_INVALID_ARG;
    uint8_t w[2 + SENSIRION_MAX_WORDS * 3];
    w[0] = (uint8_t)(cmd >> 8);
    w[1] = (uint8_t)(cmd & 0xFF);
    sensirion_pack_words(wwords, wcount, &w[2]);
    uint8_t rx[SENSIRION_MAX_WORDS * 3];
    esp_err_t err = sensor_i2c_write_delay_read(dev, w, 2 + wcount * 3, delay_ms, rx, rcount * 3);
    if (err != ESP_OK) return err;
    return sensirion_unpack_words(rx, rcount * 3, rwords, rcount);
}
