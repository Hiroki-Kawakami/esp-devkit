/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Device transport: the IDF log console doubles as the command channel. The
 * VFS driver is installed so stdin actually delivers bytes (the ROM console
 * is output-only), and each reply leaves in a single fwrite under a mutex so
 * a concurrent log line cannot split it.
 */

#include "harness_internal.h"

#include <stdio.h>
#include <string.h>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "sdkconfig.h"

#if CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG
#include "driver/usb_serial_jtag.h"
#include "driver/usb_serial_jtag_vfs.h"
#elif CONFIG_ESP_CONSOLE_UART_DEFAULT || CONFIG_ESP_CONSOLE_UART_CUSTOM
#include "driver/uart.h"
#include "driver/uart_vfs.h"
#else
#error "harness console transport needs a UART or USB-Serial-JTAG console"
#endif

static const char *TAG = "harness";

static SemaphoreHandle_t s_out_mtx;

static void console_rx_init(void) {
#if CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG
    usb_serial_jtag_driver_config_t cfg = {
        .tx_buffer_size = 4096,
        .rx_buffer_size = 2048,
    };
    ESP_ERROR_CHECK(usb_serial_jtag_driver_install(&cfg));
    usb_serial_jtag_vfs_use_driver();
#else
    ESP_ERROR_CHECK(uart_driver_install((uart_port_t)CONFIG_ESP_CONSOLE_UART_NUM,
                                        2048, 4096, 0, NULL, 0));
    uart_vfs_dev_use_driver(CONFIG_ESP_CONSOLE_UART_NUM);
#endif
    setvbuf(stdin, NULL, _IONBF, 0);
}

static int console_write(const void *data, size_t len) {
    xSemaphoreTake(s_out_mtx, portMAX_DELAY);
    size_t n = fwrite(data, 1, len, stdout);
    fflush(stdout);
    xSemaphoreGive(s_out_mtx);
    return (int)n;
}

static void console_task(void *arg) {
    (void)arg;
    console_rx_init();
    ESP_LOGI(TAG, "console harness ready");

    char line[512];
    size_t len = 0;
    for (;;) {
        int c = fgetc(stdin);
        if (c == EOF) { vTaskDelay(pdMS_TO_TICKS(20)); continue; }
        if (c == '\n' || c == '\r') {
            if (len) {
                line[len] = '\0';
                harness_handle_line(line);
                len = 0;
            }
            continue;
        }
        if (len < sizeof(line) - 1) line[len++] = (char)c;
        else len = 0;
    }
}

esp_err_t harness_transport_start(void) {
    s_out_mtx = xSemaphoreCreateMutex();
    if (!s_out_mtx) return ESP_ERR_NO_MEM;
    harness_set_writer(console_write);
    return xTaskCreate(console_task, "harness", 8192, NULL, 3, NULL) == pdPASS ? ESP_OK : ESP_FAIL;
}
