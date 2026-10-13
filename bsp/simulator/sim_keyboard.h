/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host keyboard provider fed by the SDL window's key events (sdl_panel.c),
 * always connected, supporting every mode.
 */

#pragma once
#include "bsp_keyboard.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t sim_keyboard_create(bsp_keyboard_t **out_keyboard);

/* Feed from the SDL event pump; no-ops until sim_keyboard_create.
 * `usage` is the HID usage (SDL scancode), `name` is used in CHAR mode for
 * keys that produce no text. */
void sim_keyboard_key(uint8_t usage, uint8_t modifiers, bool pressed, const char *name);
void sim_keyboard_text(const char *utf8, uint8_t modifiers);

#ifdef __cplusplus
}
#endif
