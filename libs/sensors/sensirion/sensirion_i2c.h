/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Sensirion I2C protocol framing, on top of common/sensor_i2c: 16-bit
 * big-endian commands, payloads as 16-bit words (or byte pairs) each followed
 * by CRC-8 (poly 0x31, init 0xFF). The pack / unpack helpers are pure — the
 * sim chip emulators build their wire responses with them too.
 */

#pragma once
#include <stdint.h>
#include <stddef.h>
#include "esp_err.h"
#include "sensor_i2c.h"

#ifdef __cplusplus
extern "C" {
#endif

#define SENSIRION_MAX_WORDS  16

uint8_t sensirion_crc8(const uint8_t *data, size_t len);

/* out is 3*count bytes; in is 3*count bytes (word,word,crc per triplet). */
void sensirion_pack_words(const uint16_t *words, size_t count, uint8_t *out);
void sensirion_pack_bytes(const uint8_t *data, size_t count, uint8_t *out);
esp_err_t sensirion_unpack_words(const uint8_t *in, size_t in_len, uint16_t *words, size_t count);

esp_err_t sensirion_write_cmd(i2c_master_dev_handle_t dev, uint16_t cmd);
esp_err_t sensirion_write_cmd_words(i2c_master_dev_handle_t dev, uint16_t cmd,
                                    const uint16_t *words, size_t count);
esp_err_t sensirion_read_words(i2c_master_dev_handle_t dev, uint16_t *words, size_t count);
esp_err_t sensirion_read_bytes(i2c_master_dev_handle_t dev, uint8_t *data, size_t count);
esp_err_t sensirion_cmd_read(i2c_master_dev_handle_t dev, uint16_t cmd, uint32_t delay_ms,
                             uint16_t *words, size_t count);
esp_err_t sensirion_cmd_write_read(i2c_master_dev_handle_t dev, uint16_t cmd,
                                   const uint16_t *wwords, size_t wcount, uint32_t delay_ms,
                                   uint16_t *rwords, size_t rcount);

#ifdef __cplusplus
}
#endif
