/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GT911 capacitive touch controller driver (I2C, polling). Produces a
 * bsp_touch_t provider for the common touch layer (src/bsp_touch.c).
 *
 * The driver does NOT own the I2C bus — the board initializes the bus via
 * i2c_new_master_bus() and passes the handle. The driver attaches itself with
 * i2c_master_bus_add_device(), so other devices on the same bus stay usable.
 *
 * Ported from the m5paper-bsp reference driver, trimmed to the touch path
 * (no HotKnot). Raw chip coordinates are mapped into display space via the
 * swap_xy / mirror_* config so bsp_touch_read() reports display pixels.
 */

#pragma once
#include "bsp_touch.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"

#ifdef __cplusplus
extern "C" {
#endif

/* GT911 latches its I2C address from the INT pin level at the reset rising
 * edge: INT high -> 0x14, INT low -> 0x5D. GT911_I2C_ADDR_AUTO probes both. */
#define GT911_I2C_ADDR_PRIMARY    0x14
#define GT911_I2C_ADDR_SECONDARY  0x5D
#define GT911_I2C_ADDR_AUTO       0x00

#define GT911_I2C_DEFAULT_HZ      (400 * 1000)

/* GT911 reports up to 5 simultaneous contacts. */
#define GT911_MAX_TOUCH_POINTS    5

typedef struct {
    i2c_master_bus_handle_t i2c_bus;     /* bus from i2c_new_master_bus()        */
    uint8_t                 i2c_address; /* 0x14, 0x5D, or GT911_I2C_ADDR_AUTO   */
    uint32_t                clock_hz;    /* 0 -> GT911_I2C_DEFAULT_HZ            */
    gpio_num_t              int_io;      /* INT pin; GPIO_NUM_NC to skip         */
    gpio_num_t              reset_io;    /* RESET (active LOW); GPIO_NUM_NC to skip */

    /* Orientation: raw chip (x, y) is mapped to display space as
     * swap_xy (transpose), then mirror_x / mirror_y about width / height. */
    bool                    swap_xy;
    bool                    mirror_x;
    bool                    mirror_y;
    uint16_t                width;       /* display width  (for mirror_x clamp)  */
    uint16_t                height;      /* display height (for mirror_y clamp)  */
} gt911_config_t;

/* Reset + probe the chip, attach to the bus, and return a bsp_touch_t provider.
 * Register it with bsp_touch_set_active(). */
esp_err_t gt911_touch_create(const gt911_config_t *config, bsp_touch_t **out_touch);

#ifdef __cplusplus
}
#endif
