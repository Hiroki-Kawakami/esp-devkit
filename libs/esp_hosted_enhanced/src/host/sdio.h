/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * SDIO bus access to an ESP SDIO slave (function 1 registers and data window).
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"

#define HOSTED_SDIO_BLOCK 512

esp_err_t hosted_sdio_init(void);

/* Pulses EN (no-op when not wired), then retries card init for `timeout_ms`. */
esp_err_t hosted_sdio_connect(uint32_t timeout_ms);

esp_err_t hosted_sdio_wait_int(TickType_t wait);
esp_err_t hosted_sdio_read_int(uint32_t *int_raw, uint32_t *pkt_len);
esp_err_t hosted_sdio_clear_int(uint32_t bits);
esp_err_t hosted_sdio_read_token(uint32_t *token);
esp_err_t hosted_sdio_notify(uint8_t bit);

/* `len` is the payload length; the transfer is padded to whole blocks, so
 * `buf` must hold the rounded-up size and be DMA capable. */
esp_err_t hosted_sdio_read(void *buf, size_t len);
esp_err_t hosted_sdio_write(const void *buf, size_t len);

static inline size_t hosted_sdio_padded(size_t len) {
    return (len + HOSTED_SDIO_BLOCK - 1) & ~(size_t)(HOSTED_SDIO_BLOCK - 1);
}
