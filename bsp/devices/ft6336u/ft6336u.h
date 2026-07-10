/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * FT6336U capacitive touch controller driver (I2C, polling). Produces a
 * bsp_touch_t provider for the common touch layer (src/bsp_touch.c), which owns
 * the INT ISR, orientation transform and release settle state machine, and
 * registers a bsp_dispatch source to drive polling on bsp_touch_set_active().
 *
 * The driver does NOT own the I2C bus -- the board passes the handle and the
 * driver attaches with i2c_master_bus_add_device(). RESET is a native GPIO
 * (active low); pass GPIO_NUM_NC when the line is driven elsewhere (e.g. an I/O
 * expander) and released before create(). int_io is the common layer's INT
 * descriptor: a native GPIO, GPIO_NUM_NC (poll), or BSP_TOUCH_INT_EXTERNAL when
 * the INT is aggregated on an expander and the board wakes the touch layer via
 * bsp_touch_notify(). The driver itself never touches the INT line.
 */

#pragma once
#include "bsp_touch.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

#define FT6336U_I2C_ADDR          0x38
#define FT6336U_I2C_DEFAULT_HZ    (400 * 1000)

/* FT6336U tracks up to two simultaneous contacts. */
#define FT6336U_MAX_TOUCH_POINTS  2

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint8_t                 i2c_address; /* 0 -> FT6336U_I2C_ADDR         */
    uint32_t                clock_hz;    /* 0 -> FT6336U_I2C_DEFAULT_HZ   */
    int                     int_io;      /* GPIO, GPIO_NUM_NC, or BSP_TOUCH_INT_EXTERNAL */
    gpio_num_t              reset_io;    /* RESET (active LOW); GPIO_NUM_NC to skip */

    /* Orientation: raw chip (x, y) is mapped to display space as swap_xy
     * (transpose), then mirror_x / mirror_y about width / height. */
    bool                    swap_xy;
    bool                    mirror_x;
    bool                    mirror_y;
    uint16_t                width;       /* display width  (for mirror_x clamp) */
    uint16_t                height;      /* display height (for mirror_y clamp) */
} ft6336u_config_t;

esp_err_t ft6336u_touch_create(const ft6336u_config_t *config, bsp_touch_t **out_touch);

#ifdef __cplusplus
}
#endif
