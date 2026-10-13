/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Internal keyboard-driver interface. A driver embeds bsp_keyboard_t as its
 * first member; the common layer (src/bsp_keyboard.c) owns the INT ISR,
 * connection tracking and event delivery.
 */

#pragma once
#include "bsp_types.h"

typedef struct bsp_keyboard bsp_keyboard_t;

/* int_io sentinel: events are signalled by the provider calling
 * bsp_keyboard_notify() instead of a GPIO edge. */
#define BSP_KEYBOARD_INT_EXTERNAL  (-2)

struct bsp_keyboard {
    /* Must include BSP_KEYBOARD_CAP_HID: a device whose firmware has no HID
     * output maps its keys to HID usages in the driver. */
    uint32_t caps;
    int      int_io;   /* falling edge = events pending; -1 -> polled */

    esp_err_t (*probe)(bsp_keyboard_t *self);
    /* Switch mode and discard events queued under the previous one. */
    esp_err_t (*set_mode)(bsp_keyboard_t *self, bsp_keyboard_mode_t mode);
    /* Pop up to `max` queued events of the current mode. Returning fewer than
     * `max` means the queue is empty and the INT line is released. */
    esp_err_t (*poll)(bsp_keyboard_t *self, bsp_keyboard_event_t *out, uint8_t max, uint8_t *count);
    esp_err_t (*deinit)(bsp_keyboard_t *self);
};

typedef struct {
    bsp_keyboard_mode_t   mode;
    bsp_keyboard_detect_t detect;
} bsp_keyboard_init_t;

/* Board's bsp_init() calls this once after creating its provider. */
void bsp_keyboard_set_active(bsp_keyboard_t *keyboard, const bsp_keyboard_init_t *init);

void bsp_keyboard_notify(void);
