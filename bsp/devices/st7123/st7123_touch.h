/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ST7123 capacitive touch controller driver (I2C, polling). Produces a
 * bsp_touch_t provider for the common touch layer (src/bsp_touch.c).
 *
 * The board owns the I2C bus and passes the handle here; the driver attaches
 * with i2c_master_bus_add_device(), so other devices on the same bus stay
 * usable. The chip uses 16-bit big-endian register addresses (same shape as
 * GT911).
 */

#pragma once
#include "bsp_touch.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

#define ST7123_I2C_ADDR        0x55
#define ST7123_I2C_DEFAULT_HZ  (100 * 1000)
#define ST7123_MAX_TOUCH_POINTS 5

typedef struct {
    int      task_priority;     /* > 0 to spawn a reader task that pushes to
                                   bsp_touch_set_event_cb(); 0 -> poll only */
    int      task_affinity;     /* core to pin to; < 0 -> no affinity      */
    uint32_t task_stack;        /* 0 -> default                            */
    uint32_t poll_interval_ms;  /* INT-wait fallback period; 0 -> default  */
} st7123_acquire_config_t;

typedef struct {
    i2c_master_bus_handle_t i2c_bus;
    uint32_t                clock_hz;   /* 0 -> ST7123_I2C_DEFAULT_HZ        */
    gpio_num_t              int_io;     /* INT pin; GPIO_NUM_NC to skip      */
    gpio_num_t              reset_io;   /* RESET (active LOW); NC to skip    */

    /* Orientation: raw chip (x, y) is mapped to display space as
     * swap_xy (transpose), then mirror_x / mirror_y about width / height. */
    bool                    swap_xy;
    bool                    mirror_x;
    bool                    mirror_y;
    uint16_t                width;
    uint16_t                height;

    st7123_acquire_config_t acquire;
} st7123_touch_config_t;

esp_err_t st7123_touch_create(const st7123_touch_config_t *config, bsp_touch_t **out_touch);

#ifdef __cplusplus
}
#endif
