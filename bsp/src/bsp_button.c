/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Common button layer. Providers are appended into a flat, add-order id space.
 * The lower layer is uniform: every provider is tick-driven and reports events
 * with bsp_button_emit() -- there is no built-in notion of debounce or "level"
 * here. Each provider gets its own bsp_dispatch source, so it controls its own
 * poll cadence, and a source only runs while one of its buttons has a callback.
 *
 * bsp_button_add_raw() is a convenience wrapper on top: it takes a level provider
 * (sample() only) and hangs the shared debounce + click/double-click/long-press
 * state machine off a synthesized tick, which emits into the same path.
 *
 * State machine (per raw button):
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
#include "bsp_dispatch.h"

#include <stdlib.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#define BSP_BUTTON_MAX          8
#define DEBOUNCE_MS             20
#define DEFAULT_DOUBLE_MS       300
#define DEFAULT_LONG_MS         800
#define POLL_INTERVAL_MS        10

/* Per-id callback + timing registry (the user-facing state, keyed by global id). */
typedef struct {
    bsp_button_cb_t cb_down, cb_up, cb_click, cb_double, cb_long;
    void *arg_down, *arg_up, *arg_click, *arg_double, *arg_long;
    uint16_t double_ms;
    uint16_t long_ms;
} cb_entry_t;

static cb_entry_t s_cb[BSP_BUTTON_MAX];
static uint8_t    s_count;   /* total buttons across providers */

typedef struct provider_node {
    bsp_button_t         *provider;
    bsp_dispatch_source_t source;
    struct provider_node *next;
} provider_node_t;

static provider_node_t *s_providers;

static inline void fire(bsp_button_cb_t cb, void *arg, uint8_t id) {
    if (cb) cb(id, arg);
}
static inline uint32_t ms_since(TickType_t then, TickType_t now) {
    return (uint32_t)((now - then) * portTICK_PERIOD_MS);
}

void bsp_button_emit(uint8_t id, bsp_button_event_t event) {
    if (id >= s_count) return;
    cb_entry_t *e = &s_cb[id];
    switch (event) {
        case BSP_BUTTON_EVENT_DOWN:         fire(e->cb_down,   e->arg_down,   id); break;
        case BSP_BUTTON_EVENT_UP:           fire(e->cb_up,     e->arg_up,     id); break;
        case BSP_BUTTON_EVENT_CLICK:        fire(e->cb_click,  e->arg_click,  id); break;
        case BSP_BUTTON_EVENT_DOUBLE_CLICK: fire(e->cb_double, e->arg_double, id); break;
        case BSP_BUTTON_EVENT_LONG_PRESS:   fire(e->cb_long,   e->arg_long,   id); break;
    }
}

static bool any_registered(uint8_t base, uint8_t count) {
    for (uint8_t i = base; i < base + count; i++) {
        const cb_entry_t *e = &s_cb[i];
        if (e->cb_down || e->cb_up || e->cb_click || e->cb_double || e->cb_long) return true;
    }
    return false;
}

/* Gate + dispatch every provider's tick: skip (idle) until one of its buttons has
 * a callback, so a source never polls hardware nobody is listening to. */
static uint32_t provider_tick(void *ctx) {
    bsp_button_t *p = ctx;
    if (!any_registered(p->base_id, p->count)) return BSP_DISPATCH_IDLE;
    return p->tick(p);
}

static provider_node_t *node_for_id(uint8_t id) {
    for (provider_node_t *n = s_providers; n; n = n->next) {
        uint8_t base = n->provider->base_id;
        if (id >= base && id < base + n->provider->count) return n;
    }
    return NULL;
}

/* A registration for `id` changed: push the new snapshot to the owning provider
 * (if it wants config) and wake its source so it leaves idle. */
static void register_changed(uint8_t id) {
    provider_node_t *n = node_for_id(id);
    if (!n) return;
    if (n->provider->on_config) {
        const cb_entry_t *e = &s_cb[id];
        const bsp_button_reg_t reg = {
            .down         = e->cb_down   != NULL,
            .up           = e->cb_up     != NULL,
            .click        = e->cb_click  != NULL,
            .double_click = e->cb_double != NULL,
            .long_press   = e->cb_long   != NULL,
            .double_ms    = e->double_ms,
            .long_ms      = e->long_ms,
        };
        n->provider->on_config(n->provider, id - n->provider->base_id, &reg);
    }
    bsp_dispatch_notify(&n->source);
}

esp_err_t bsp_button_add(bsp_button_t *provider) {
    if (!provider || !provider->tick || provider->count == 0) return ESP_ERR_INVALID_ARG;
    if (s_count + provider->count > BSP_BUTTON_MAX) return ESP_ERR_NO_MEM;

    provider_node_t *node = calloc(1, sizeof(*node));
    if (!node) return ESP_ERR_NO_MEM;

    provider->base_id = s_count;
    provider->source  = &node->source;
    for (uint8_t i = 0; i < provider->count; i++) {
        s_cb[s_count + i].double_ms = DEFAULT_DOUBLE_MS;
        s_cb[s_count + i].long_ms   = DEFAULT_LONG_MS;
    }
    s_count += provider->count;

    node->provider    = provider;
    node->source.tick = provider_tick;
    node->source.ctx  = provider;
    node->next        = s_providers;
    s_providers       = node;

    bsp_dispatch_add_source(&node->source);
    return ESP_OK;
}

/* --- Shared state-machine wrapper (bsp_button_add_raw) ----------------------- */

typedef enum {
    BTN_IDLE = 0,
    BTN_PRESSED,
    BTN_PRESSED_LONG,
    BTN_WAIT_DOUBLE,
    BTN_PRESSED_AGAIN,
} btn_state_t;

typedef struct {
    bool        stable_pressed;
    bool        raw_differs;
    TickType_t  raw_diff_tick;
    btn_state_t state;
    TickType_t  press_tick;
    TickType_t  release_tick;
} button_sm_t;

typedef struct {
    bsp_button_t      base;   /* tick-driven provider registered via bsp_button_add */
    bsp_button_raw_t *raw;    /* wrapped level provider */
    button_sm_t       sm[];   /* one per button */
} raw_wrapper_t;

/* Advance one button from its raw level; emit events; return true when settled. */
static bool sm_step(button_sm_t *b, uint8_t id, bool raw) {
    cb_entry_t *e = &s_cb[id];
    TickType_t now = xTaskGetTickCount();

    /* Time-based debounce: raw must differ from stable for DEBOUNCE_MS. */
    bool edge = false;
    if (raw == b->stable_pressed) {
        b->raw_differs = false;
    } else if (!b->raw_differs) {
        b->raw_differs   = true;
        b->raw_diff_tick = now;
    } else if (ms_since(b->raw_diff_tick, now) >= DEBOUNCE_MS) {
        b->stable_pressed = raw;
        b->raw_differs    = false;
        edge = true;
    }

    if (edge) {
        if (raw) {
            bsp_button_emit(id, BSP_BUTTON_EVENT_DOWN);
            b->press_tick = now;
            if (b->state == BTN_WAIT_DOUBLE) {
                bsp_button_emit(id, BSP_BUTTON_EVENT_DOUBLE_CLICK);
                b->state = BTN_PRESSED_AGAIN;
            } else {
                b->state = BTN_PRESSED;
            }
        } else {
            bsp_button_emit(id, BSP_BUTTON_EVENT_UP);
            if (b->state == BTN_PRESSED) {
                if (e->cb_double) {
                    b->state = BTN_WAIT_DOUBLE;
                    b->release_tick = now;
                } else {
                    bsp_button_emit(id, BSP_BUTTON_EVENT_CLICK);
                    b->state = BTN_IDLE;
                }
            } else {
                b->state = BTN_IDLE;
            }
        }
    }

    switch (b->state) {
        case BTN_PRESSED:
            if (e->cb_long && e->long_ms > 0 && ms_since(b->press_tick, now) >= e->long_ms) {
                bsp_button_emit(id, BSP_BUTTON_EVENT_LONG_PRESS);
                b->state = BTN_PRESSED_LONG;
            }
            break;
        case BTN_WAIT_DOUBLE:
            if (ms_since(b->release_tick, now) >= e->double_ms) {
                bsp_button_emit(id, BSP_BUTTON_EVENT_CLICK);
                b->state = BTN_IDLE;
            }
            break;
        default:
            break;
    }
    return b->state == BTN_IDLE && !b->raw_differs;
}

static uint32_t raw_tick(bsp_button_t *self) {
    raw_wrapper_t *w = (raw_wrapper_t *)self;
    const uint8_t n = self->count;

    bool pressed[BSP_BUTTON_MAX] = {0};
    if (w->raw->sample(w->raw, pressed, n) != ESP_OK) return POLL_INTERVAL_MS;

    bool all_settled = true;
    for (uint8_t i = 0; i < n; i++) {
        if (!sm_step(&w->sm[i], self->base_id + i, pressed[i])) all_settled = false;
    }

    if (!all_settled) return POLL_INTERVAL_MS;           /* a debounce/timer is pending */
    if (w->raw->has_int) return BSP_DISPATCH_IDLE;       /* wait for the next edge ISR */
    return w->raw->poll_ms ? w->raw->poll_ms : POLL_INTERVAL_MS;
}

static esp_err_t raw_deinit(bsp_button_t *self) {
    raw_wrapper_t *w = (raw_wrapper_t *)self;
    if (w->raw && w->raw->deinit) w->raw->deinit(w->raw);
    free(w);
    return ESP_OK;
}

esp_err_t bsp_button_add_raw(bsp_button_raw_t *raw) {
    if (!raw || !raw->sample || raw->count == 0) return ESP_ERR_INVALID_ARG;

    raw_wrapper_t *w = calloc(1, sizeof(*w) + raw->count * sizeof(button_sm_t));
    if (!w) return ESP_ERR_NO_MEM;
    w->raw          = raw;
    w->base.count   = raw->count;
    w->base.tick    = raw_tick;
    w->base.deinit  = raw_deinit;

    esp_err_t err = bsp_button_add(&w->base);
    if (err != ESP_OK) {
        free(w);
        return err;
    }
    /* The wrapper owns the registered source; point the raw provider at it so its
     * edge ISR wakes the right one. */
    raw->source = w->base.source;
    return ESP_OK;
}

void bsp_button_notify_from_isr(bsp_button_raw_t *self, BaseType_t *hp) {
    if (self && self->source) bsp_dispatch_notify_from_isr(self->source, hp);
}

/* --- Public API (flat id space) --------------------------------------------- */

uint8_t bsp_button_count(void) { return s_count; }

void bsp_button_on_down(uint8_t id, bsp_button_cb_t cb, void *arg) {
    if (id >= s_count) return;
    s_cb[id].cb_down  = cb;
    s_cb[id].arg_down = arg;
    register_changed(id);
}
void bsp_button_on_up(uint8_t id, bsp_button_cb_t cb, void *arg) {
    if (id >= s_count) return;
    s_cb[id].cb_up  = cb;
    s_cb[id].arg_up = arg;
    register_changed(id);
}
void bsp_button_on_click(uint8_t id, bsp_button_cb_t cb, void *arg) {
    if (id >= s_count) return;
    s_cb[id].cb_click  = cb;
    s_cb[id].arg_click = arg;
    register_changed(id);
}
void bsp_button_on_double_click(uint8_t id, uint16_t interval_ms,
                                bsp_button_cb_t cb, void *arg) {
    if (id >= s_count) return;
    s_cb[id].cb_double  = cb;
    s_cb[id].arg_double = arg;
    s_cb[id].double_ms  = interval_ms ? interval_ms : DEFAULT_DOUBLE_MS;
    register_changed(id);
}
void bsp_button_on_long_press(uint8_t id, uint16_t duration_ms,
                              bsp_button_cb_t cb, void *arg) {
    if (id >= s_count) return;
    s_cb[id].cb_long  = cb;
    s_cb[id].arg_long = arg;
    s_cb[id].long_ms  = duration_ms ? duration_ms : DEFAULT_LONG_MS;
    register_changed(id);
}
