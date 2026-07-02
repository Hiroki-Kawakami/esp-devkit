/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Common button layer: dispatches sample() through the active provider vtable
 * and runs the per-button state machine that turns debounced press/release
 * edges into click / double-click / long-press events. Registered as one
 * source of the shared bsp_input task.
 *
 * State transitions:
 *   IDLE            +down       -> PRESSED; fire DOWN
 *   PRESSED         +up (short) -> WAIT_DOUBLE (if double registered),
 *                                  else fire CLICK -> IDLE. UP fires either way.
 *   PRESSED         +long timer -> PRESSED_LONG; fire LONG_PRESS
 *   PRESSED_LONG    +up         -> IDLE; fire UP  (click/double suppressed)
 *   WAIT_DOUBLE     +down       -> PRESSED_AGAIN; fire DOWN + DOUBLE_CLICK
 *   WAIT_DOUBLE     +timeout    -> IDLE; fire CLICK if registered, else silent
 *   PRESSED_AGAIN   +up         -> IDLE; fire UP  (no further click)
 */

#include "bsp.h"
#include "bsp_button.h"
#include "bsp_input.h"

#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#define BSP_BUTTON_MAX          8
#define DEFAULT_DEBOUNCE_MS     20
#define DEFAULT_DOUBLE_MS       300
#define DEFAULT_LONG_MS         800

typedef enum {
    BTN_IDLE = 0,
    BTN_PRESSED,
    BTN_PRESSED_LONG,
    BTN_WAIT_DOUBLE,
    BTN_PRESSED_AGAIN,
} btn_state_t;

typedef struct {
    bsp_button_cb_t cb_down, cb_up, cb_click, cb_double, cb_long;
    void *arg_down, *arg_up, *arg_click, *arg_double, *arg_long;
    uint16_t double_ms;
    uint16_t long_ms;

    bool       stable_pressed;
    bool       raw_differs;
    TickType_t raw_diff_tick;

    btn_state_t state;
    TickType_t  press_tick;
    TickType_t  release_tick;
} button_state_t;

static bsp_button_t   *s_button;
static button_state_t  s_state[BSP_BUTTON_MAX];
static uint16_t        s_debounce_ms = DEFAULT_DEBOUNCE_MS;
static bool            s_source_registered;

static inline void fire(bsp_button_cb_t cb, void *arg, uint8_t id) {
    if (cb) cb(id, arg);
}
static inline uint32_t ticks_to_ms(TickType_t t) {
    return (uint32_t)(t * portTICK_PERIOD_MS);
}

static inline uint8_t active_count(void) {
    if (!s_button) return 0;
    return s_button->count > BSP_BUTTON_MAX ? BSP_BUTTON_MAX : s_button->count;
}

static void advance_one(uint8_t id, bool raw) {
    button_state_t *b = &s_state[id];
    TickType_t now = xTaskGetTickCount();

    /* Time-based debounce: raw must persistently differ from stable for
     * debounce_ms of wall time before we accept the edge. */
    bool edge = false;
    if (raw == b->stable_pressed) {
        b->raw_differs = false;
    } else {
        if (!b->raw_differs) {
            b->raw_differs   = true;
            b->raw_diff_tick = now;
        } else if (ticks_to_ms(now - b->raw_diff_tick) >= s_debounce_ms) {
            b->stable_pressed = raw;
            b->raw_differs    = false;
            edge = true;
        }
    }

    if (edge) {
        if (raw) {
            fire(b->cb_down, b->arg_down, id);
            b->press_tick = now;
            if (b->state == BTN_WAIT_DOUBLE) {
                fire(b->cb_double, b->arg_double, id);
                b->state = BTN_PRESSED_AGAIN;
            } else {
                b->state = BTN_PRESSED;
            }
        } else {
            fire(b->cb_up, b->arg_up, id);
            switch (b->state) {
                case BTN_PRESSED:
                    if (b->cb_double) {
                        b->state = BTN_WAIT_DOUBLE;
                        b->release_tick = now;
                    } else {
                        fire(b->cb_click, b->arg_click, id);
                        b->state = BTN_IDLE;
                    }
                    break;
                default:
                    b->state = BTN_IDLE;
                    break;
            }
        }
    }

    switch (b->state) {
        case BTN_PRESSED:
            if (b->cb_long && b->long_ms > 0 &&
                ticks_to_ms(now - b->press_tick) >= b->long_ms) {
                fire(b->cb_long, b->arg_long, id);
                b->state = BTN_PRESSED_LONG;
            }
            break;
        case BTN_WAIT_DOUBLE:
            if (ticks_to_ms(now - b->release_tick) >= b->double_ms) {
                fire(b->cb_click, b->arg_click, id);
                b->state = BTN_IDLE;
            }
            break;
        default: break;
    }
}

/* Providers with no INT (e.g. an ADC ladder) can't wake a dormant task, so
 * this source always requests another tick while a provider is active. */
static bool button_tick(void *ctx) {
    (void)ctx;
    if (!s_button || !s_button->sample) return false;

    const uint8_t n = active_count();
    if (n == 0) return false;

    bool pressed[BSP_BUTTON_MAX] = {0};
    if (s_button->sample(s_button, pressed, n) == ESP_OK) {
        for (uint8_t i = 0; i < n; i++) advance_one(i, pressed[i]);
    }
    return true;
}

void bsp_button_set_active(bsp_button_t *button) {
    s_button = button;
    if (!button) return;

    const uint8_t n = active_count();
    for (uint8_t i = 0; i < n; i++) {
        s_state[i] = (button_state_t){
            .double_ms = DEFAULT_DOUBLE_MS,
            .long_ms   = DEFAULT_LONG_MS,
        };
    }

    if (!s_source_registered) {
        bsp_input_source_t src = { .tick = button_tick, .ctx = NULL };
        if (bsp_input_add_source(&src) == ESP_OK) s_source_registered = true;
    }
}

uint8_t bsp_button_count(void) { return active_count(); }

void bsp_button_on_down(uint8_t id, bsp_button_cb_t cb, void *arg) {
    if (id >= active_count()) return;
    s_state[id].cb_down  = cb;
    s_state[id].arg_down = arg;
}
void bsp_button_on_up(uint8_t id, bsp_button_cb_t cb, void *arg) {
    if (id >= active_count()) return;
    s_state[id].cb_up  = cb;
    s_state[id].arg_up = arg;
}
void bsp_button_on_click(uint8_t id, bsp_button_cb_t cb, void *arg) {
    if (id >= active_count()) return;
    s_state[id].cb_click  = cb;
    s_state[id].arg_click = arg;
}
void bsp_button_on_double_click(uint8_t id, uint16_t interval_ms,
                                bsp_button_cb_t cb, void *arg) {
    if (id >= active_count()) return;
    s_state[id].cb_double  = cb;
    s_state[id].arg_double = arg;
    s_state[id].double_ms  = interval_ms ? interval_ms : DEFAULT_DOUBLE_MS;
}
void bsp_button_on_long_press(uint8_t id, uint16_t duration_ms,
                              bsp_button_cb_t cb, void *arg) {
    if (id >= active_count()) return;
    s_state[id].cb_long  = cb;
    s_state[id].arg_long = arg;
    s_state[id].long_ms  = duration_ms ? duration_ms : DEFAULT_LONG_MS;
}
