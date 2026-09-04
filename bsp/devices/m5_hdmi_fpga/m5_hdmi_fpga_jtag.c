/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Ported from LovyanGFX (Panel_M5HDMI::LOAD_FPGA), Copyright (c) lovyan03,
 * licensed under the FreeBSD License. The bitstream it writes is vendored
 * separately (vendor/fpga_bitstream_rle.h).
 */

#include "m5_hdmi_fpga_jtag.h"
#include "sdkconfig.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#ifdef CONFIG_BSP_M5_HDMI_FPGA_BITSTREAM
#include "vendor/fpga_bitstream_rle.h"

static const char *TAG = "m5_hdmi_fpga";

/* Gowin JTAG instructions. */
#define ISC_NOOP      0x02
#define ISC_ERASE     0x05
#define ERASE_DONE    0x09
#define ISC_ENABLE    0x15
#define FAST_PROGRAM  0x17
#define STATUS_CODE   0x41
#define ISC_DISABLE   0x3A

/* Status bit that stays set while the FPGA is still coming up after power-on;
 * the wait is needed because it clears at different times per device lot. */
#define STATUS_BUSY   0x200

typedef enum {
    TAP_RESET,
    TAP_IDLE,
    TAP_DRSHIFT,
    TAP_DREXIT1,
    TAP_IRSHIFT,
    TAP_IREXIT1,
    TAP_UNKNOWN,
} tap_state_t;

typedef struct {
    gpio_num_t tck, tdi, tdo, tms;
} jtag_t;

static void tap_move_inner(const jtag_t *j, bool tms_value, size_t clock_count) {
    gpio_set_level(j->tms, tms_value);
    do {
        gpio_set_level(j->tck, 0);
        gpio_set_level(j->tck, 1);
    } while (--clock_count);
}

static void tap_move(const jtag_t *j, tap_state_t from, tap_state_t to) {
    int high = 1;
    int low = 2;
    if (from == TAP_UNKNOWN && to == TAP_IDLE) {
        high = 8;
    } else if (from == TAP_IDLE && to == TAP_IDLE) {
        high = 0;
        low = 3;
    } else if (from == TAP_IDLE && to == TAP_IRSHIFT) {
        high = 2;
    } else if (from == TAP_IDLE && to == TAP_DRSHIFT) {
    } else if (from == TAP_IREXIT1 && to == TAP_IDLE) {
        low = 11;
    } else if (from == TAP_DREXIT1 && to == TAP_IDLE) {
    } else {
        ESP_LOGE(TAG, "bad tap walk %d -> %d", from, to);
        return;
    }
    if (high) tap_move_inner(j, true, high);
    tap_move_inner(j, false, low);
}

/* Shifts `din` MSB-first `len` times. `tms` raises TMS on the very last bit to
 * leave the shift state; `lsb` reverses the byte first (instructions are shifted
 * LSB-first). */
static void jtag_write(const jtag_t *j, uint8_t din, bool tms, bool lsb, size_t len) {
    if (lsb && din) {
        din = ((din & 0x55) << 1) + ((din >> 1) & 0x55);
        din = ((din & 0x33) << 2) + ((din >> 2) & 0x33);
        din = ((din & 0x0f) << 4) + ((din >> 4) & 0x0f);
    }
    gpio_set_level(j->tms, 0);
    int prev = -1;
    do {
        size_t i = 8;
        do {
            if (!--i && tms && len == 1) gpio_set_level(j->tms, 1);
            int level = (din >> i) & 1;
            if (prev != level) {
                prev = level;
                gpio_set_level(j->tdi, level);
            }
            gpio_set_level(j->tck, 0);
            gpio_set_level(j->tck, 1);
        } while (i);
    } while (--len);
}

static void write_inst(const jtag_t *j, uint8_t inst) {
    tap_move(j, TAP_IDLE, TAP_IRSHIFT);
    jtag_write(j, inst, true, true, 1);
    tap_move(j, TAP_IREXIT1, TAP_IDLE);
}

static void dummy_clock(const jtag_t *j, uint32_t ms) {
    int64_t deadline = esp_timer_get_time() + (int64_t)ms * 1000;
    do {
        gpio_set_level(j->tck, 0);
        gpio_set_level(j->tck, 1);
        gpio_set_level(j->tck, 0);
        gpio_set_level(j->tck, 1);
    } while (esp_timer_get_time() < deadline);
}

static uint32_t read_status(const jtag_t *j) {
    uint32_t out = 0;
    write_inst(j, STATUS_CODE);
    tap_move(j, TAP_IDLE, TAP_DRSHIFT);

    gpio_set_level(j->tms, 0);
    for (size_t i = 0; i < 32; i++) {
        if (i == 31) gpio_set_level(j->tms, 1);
        gpio_set_level(j->tck, 0);
        gpio_set_level(j->tck, 1);
        if (gpio_get_level(j->tdo)) out |= 1u << i;
    }
    tap_move(j, TAP_DREXIT1, TAP_IDLE);
    return out;
}

static void write_bitstream(const jtag_t *j) {
    int32_t run_len = -1;
    int32_t literal_len = -1;
    for (size_t i = 0; i < sizeof(M5_HDMI_FPGA_BITSTREAM_RLE); i++) {
        const bool last = (i == sizeof(M5_HDMI_FPGA_BITSTREAM_RLE) - 1);
        const uint8_t byte = M5_HDMI_FPGA_BITSTREAM_RLE[i];

        if (run_len < 0) {
            run_len = byte;
            literal_len = -1;
        } else if (run_len == 0) {
            if (literal_len == -1) {
                literal_len = byte;
            } else {
                jtag_write(j, byte, last, false, 1);
                if (--literal_len == 0) run_len = -1;
            }
        } else {
            jtag_write(j, byte, last, false, run_len);
            run_len = -1;
        }
    }
}

esp_err_t m5_hdmi_fpga_jtag_load(gpio_num_t tck_io, gpio_num_t tdi_io,
                                 gpio_num_t tdo_io, gpio_num_t tms_io) {
    const jtag_t j = { .tck = tck_io, .tdi = tdi_io, .tdo = tdo_io, .tms = tms_io };

    gpio_set_direction(j.tck, GPIO_MODE_OUTPUT);
    gpio_set_direction(j.tdi, GPIO_MODE_OUTPUT);
    gpio_set_direction(j.tms, GPIO_MODE_OUTPUT);
    gpio_set_direction(j.tdo, GPIO_MODE_INPUT);
    gpio_set_level(j.tms, 0);
    gpio_set_level(j.tdi, 0);
    gpio_set_level(j.tck, 0);

    int retry = 128;
    while (retry && (read_status(&j) & STATUS_BUSY)) {
        vTaskDelay(pdMS_TO_TICKS(1));
        retry--;
    }
    if (!retry) {
        ESP_LOGE(TAG, "FPGA stayed busy; not loading");
        return ESP_ERR_TIMEOUT;
    }

    tap_move(&j, TAP_UNKNOWN, TAP_IDLE);

    write_inst(&j, ISC_ENABLE);
    write_inst(&j, ISC_ERASE);
    write_inst(&j, ISC_NOOP);
    dummy_clock(&j, 4);
    write_inst(&j, ERASE_DONE);
    write_inst(&j, ISC_NOOP);
    write_inst(&j, ISC_DISABLE);
    write_inst(&j, ISC_NOOP);
    dummy_clock(&j, 4);

    write_inst(&j, ISC_ENABLE);
    write_inst(&j, FAST_PROGRAM);
    tap_move(&j, TAP_IDLE, TAP_DRSHIFT);
    write_bitstream(&j);
    tap_move(&j, TAP_DREXIT1, TAP_IDLE);
    write_inst(&j, ISC_DISABLE);
    write_inst(&j, ISC_NOOP);

    ESP_LOGI(TAG, "bitstream written to FPGA SRAM");
    return ESP_OK;
}

#else  /* CONFIG_BSP_M5_HDMI_FPGA_BITSTREAM */

esp_err_t m5_hdmi_fpga_jtag_load(gpio_num_t tck_io, gpio_num_t tdi_io,
                                 gpio_num_t tdo_io, gpio_num_t tms_io) {
    (void)tck_io;
    (void)tdi_io;
    (void)tdo_io;
    (void)tms_io;
    return ESP_ERR_NOT_SUPPORTED;
}

#endif
