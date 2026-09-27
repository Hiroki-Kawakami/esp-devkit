/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * esp-hosted compatible Wi-Fi coprocessor over SDIO.
 */

#include "bench.h"
#include "board.h"
#include "esp_event.h"
#include "esp_log.h"
#include "logfwd.h"
#include "nvs_flash.h"
#include "recovery.h"
#include "rpc.h"
#include "serial.h"
#include "transport.h"
#include "wifi.h"

static void on_open_safe(void) {
    recovery_clear();
    rpc_send_esp_init();
}

void app_main(void) {
    logfwd_init();
    bool safe = recovery_check();
    board_init();

    if (!safe) {
        esp_err_t err = nvs_flash_init();
        if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
            nvs_flash_erase();
            err = nvs_flash_init();
        }
        ESP_ERROR_CHECK(err);
        ESP_ERROR_CHECK(esp_event_loop_create_default());
    }

    if (!safe) bench_init();
    rpc_init(safe);
    serial_init(rpc_on_message);
    const transport_cbs_t cbs = {
        .on_sta = safe ? NULL : wifi_on_host_packet,
        .on_serial = serial_on_packet,
        .on_test = safe ? NULL : bench_on_packet,
        .on_log_enable = logfwd_enable,
        .on_open = safe ? on_open_safe : rpc_send_esp_init,
        .ext_caps = HOSTED_EXT_CAP_LOG | HOSTED_EXT_CAP_RX_AGGR | (safe ? 0 : HOSTED_EXT_CAP_TEST),
    };
    ESP_ERROR_CHECK(transport_init(&cbs));
}
