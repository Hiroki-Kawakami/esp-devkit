/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Coprocessor control beside the esp_wifi API (which esp_wifi_init() brings up).
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "driver/sdmmc_types.h"
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

/* HOSTED_EXT_CAP_* (hosted_wire.h) the coprocessor advertised. */
uint32_t hosted_host_ext_caps(void);

/* Coprocessor log lines appear on the host console prefixed with "[C6]". */
esp_err_t hosted_host_log_forward(bool enable);

/* Test channel (HOSTED_IF_TEST); `cb` runs on the transport rx task. */
typedef void (*hosted_host_test_rx_t)(const uint8_t *data, size_t len, void *arg);
void hosted_host_set_test_rx(hosted_host_test_rx_t cb, void *arg);
esp_err_t hosted_host_test_send(const void *data, size_t len, uint32_t wait_ms);

/* The SDMMC controller is shared. Another slot driven by the IDF sdmmc driver
 * uses these as its sdmmc_host_t.init and .do_transaction, and brackets
 * whatever else touches the controller (mount, slot init/deinit, clock
 * changes) with acquire/release, which nest. */
esp_err_t hosted_host_sdmmc_init(void);
esp_err_t hosted_host_sdmmc_do_transaction(int slot, sdmmc_command_t *cmd);
void hosted_host_sdmmc_acquire(void);
void hosted_host_sdmmc_release(void);

#ifdef __cplusplus
}
#endif
