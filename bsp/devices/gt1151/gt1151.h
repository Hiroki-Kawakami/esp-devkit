/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GT1151 capacitive touch controller driver (I2C, polling). Produces a
 * bsp_touch_t provider for the common touch layer (src/bsp_touch.c), which owns
 * the reader task, INT ISR, orientation transform, and the release settle
 * state machine -- start it with bsp_touch_start_reader() after set_active.
 *
 * Register layout matches the GT911 (Goodix protocol family): status @ 0x814E,
 * touch data @ 0x814F (8 bytes per point), product ID @ 0x8140. Main
 * differences from GT911: up to 10 concurrent contacts, no HotKnot subsystem.
 *
 * The driver does NOT own the I2C bus -- the board initializes the bus via
 * i2c_new_master_bus() and passes the handle. Raw chip coordinates are reported
 * through bsp_touch_raw_point_t; the common layer maps them into display space.
 */

#pragma once
#include "bsp_touch.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

/* GT1151 latches its I2C address from the INT pin level at the reset rising
 * edge: INT high -> 0x14, INT low -> 0x5D. GT1151_I2C_ADDR_AUTO probes both. */
#define GT1151_I2C_ADDR_PRIMARY    0x14
#define GT1151_I2C_ADDR_SECONDARY  0x5D
#define GT1151_I2C_ADDR_AUTO       0x00

#define GT1151_I2C_DEFAULT_HZ      (400 * 1000)

/* GT1151 reports up to 10 simultaneous contacts. */
#define GT1151_MAX_TOUCH_POINTS    10

typedef struct {
    i2c_master_bus_handle_t i2c_bus;     /* bus from i2c_new_master_bus()        */
    uint8_t                 i2c_address; /* 0x14, 0x5D, or GT1151_I2C_ADDR_AUTO  */
    uint32_t                clock_hz;    /* 0 -> GT1151_I2C_DEFAULT_HZ           */
    gpio_num_t              int_io;      /* INT pin; GPIO_NUM_NC to skip         */
    gpio_num_t              reset_io;    /* RESET (active LOW); GPIO_NUM_NC to skip */

    /* Orientation: raw chip (x, y) is mapped to display space as
     * swap_xy (transpose), then mirror_x / mirror_y about width / height. */
    bool                    swap_xy;
    bool                    mirror_x;
    bool                    mirror_y;
    uint16_t                width;       /* display width  (for mirror_x clamp)  */
    uint16_t                height;      /* display height (for mirror_y clamp)  */
} gt1151_config_t;

/* Reset + probe the chip, attach to the bus, and return a bsp_touch_t provider.
 * Register it with bsp_touch_set_active() and then call bsp_touch_start_reader(). */
esp_err_t gt1151_touch_create(const gt1151_config_t *config, bsp_touch_t **out_touch);

#ifdef __cplusplus
}
#endif
