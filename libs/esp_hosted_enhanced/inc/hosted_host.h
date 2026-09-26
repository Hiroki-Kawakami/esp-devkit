/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Coprocessor control beside the esp_wifi API (which esp_wifi_init() brings up).
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Resets the coprocessor and waits for its handshake, without Wi-Fi. */
esp_err_t hosted_host_connect(void);

esp_err_t hosted_host_fw_version(uint32_t *major, uint32_t *minor, uint32_t *patch);

/* The image goes to the coprocessor's inactive OTA slot; activate switches to
 * it and the coprocessor restarts shortly after replying. */
esp_err_t hosted_host_ota_begin(void);
esp_err_t hosted_host_ota_write(const void *data, size_t len);
esp_err_t hosted_host_ota_end(void);
esp_err_t hosted_host_ota_activate(void);

#ifdef __cplusplus
}
#endif
