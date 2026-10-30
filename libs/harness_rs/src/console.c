/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "console.h"

#include <stdio.h>

#ifdef ESP_PLATFORM
#include "esp_err.h"
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
#endif

void harness_rs_console_init(void) {
#ifdef ESP_PLATFORM
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
#endif
}

int harness_rs_console_getc(void) {
    return fgetc(stdin);
}

void harness_rs_console_write(const void *data, size_t len) {
    fwrite(data, 1, len, stdout);
    fflush(stdout);
}
