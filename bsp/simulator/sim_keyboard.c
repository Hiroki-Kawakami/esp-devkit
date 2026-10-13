/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sim_keyboard.h"

#include <stdlib.h>
#include <string.h>

#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

#define QUEUE_LEN 32
#define HID_USAGE_MODIFIER_FIRST 0xE0
#define HID_USAGE_MODIFIER_LAST  0xE7

typedef struct {
    bsp_keyboard_t       base;
    SemaphoreHandle_t    lock;
    bsp_keyboard_mode_t  mode;
    bsp_keyboard_event_t queue[QUEUE_LEN];
    uint8_t              head, count;
} sim_keyboard_t;

static sim_keyboard_t *s_kb;

static void push(const bsp_keyboard_event_t *event) {
    xSemaphoreTake(s_kb->lock, portMAX_DELAY);
    if (s_kb->count < QUEUE_LEN) {
        s_kb->queue[(s_kb->head + s_kb->count) % QUEUE_LEN] = *event;
        s_kb->count++;
    }
    xSemaphoreGive(s_kb->lock);
    bsp_keyboard_notify();
}

static void push_char(const char *str, uint8_t modifiers) {
    bsp_keyboard_event_t event = { .type = BSP_KEYBOARD_EVENT_CHAR };
    size_t len = strlen(str);
    if (len > BSP_KEYBOARD_CHAR_MAX) len = BSP_KEYBOARD_CHAR_MAX;
    event.chr.modifiers = modifiers;
    event.chr.len       = (uint8_t)len;
    memcpy(event.chr.str, str, len);
    push(&event);
}

void sim_keyboard_key(uint8_t usage, uint8_t modifiers, bool pressed, const char *name) {
    if (!s_kb) return;
    switch (s_kb->mode) {
        case BSP_KEYBOARD_MODE_HID: {
            bool modifier_key = usage >= HID_USAGE_MODIFIER_FIRST && usage <= HID_USAGE_MODIFIER_LAST;
            push(&(bsp_keyboard_event_t){
                .type = BSP_KEYBOARD_EVENT_HID,
                .hid  = { .modifiers = modifiers, .keycode = (pressed && !modifier_key) ? usage : 0 },
            });
            break;
        }
        case BSP_KEYBOARD_MODE_RAW:
            push(&(bsp_keyboard_event_t){
                .type = BSP_KEYBOARD_EVENT_RAW,
                .raw  = { .index = usage, .pressed = pressed },
            });
            break;
        case BSP_KEYBOARD_MODE_CHAR:
            if (pressed && name) push_char(name, modifiers);
            break;
    }
}

void sim_keyboard_text(const char *utf8, uint8_t modifiers) {
    if (!s_kb || s_kb->mode != BSP_KEYBOARD_MODE_CHAR) return;
    push_char(utf8, modifiers);
}

static esp_err_t sim_probe(bsp_keyboard_t *self) {
    (void)self;
    return ESP_OK;
}

static esp_err_t sim_set_mode(bsp_keyboard_t *self, bsp_keyboard_mode_t mode) {
    sim_keyboard_t *s = (sim_keyboard_t *)self;
    xSemaphoreTake(s->lock, portMAX_DELAY);
    s->mode  = mode;
    s->count = 0;
    xSemaphoreGive(s->lock);
    return ESP_OK;
}

static esp_err_t sim_poll(bsp_keyboard_t *self, bsp_keyboard_event_t *out, uint8_t max, uint8_t *count) {
    sim_keyboard_t *s = (sim_keyboard_t *)self;
    uint8_t n = 0;
    xSemaphoreTake(s->lock, portMAX_DELAY);
    while (n < max && s->count > 0) {
        out[n++] = s->queue[s->head];
        s->head = (s->head + 1) % QUEUE_LEN;
        s->count--;
    }
    xSemaphoreGive(s->lock);
    *count = n;
    return ESP_OK;
}

static esp_err_t sim_deinit(bsp_keyboard_t *self) {
    sim_keyboard_t *s = (sim_keyboard_t *)self;
    if (s == s_kb) s_kb = NULL;
    vSemaphoreDelete(s->lock);
    free(s);
    return ESP_OK;
}

esp_err_t sim_keyboard_create(bsp_keyboard_t **out_keyboard) {
    if (!out_keyboard) return ESP_ERR_INVALID_ARG;
    sim_keyboard_t *s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;
    s->lock = xSemaphoreCreateMutex();
    if (!s->lock) {
        free(s);
        return ESP_ERR_NO_MEM;
    }
    s->base.caps     = BSP_KEYBOARD_CAP_HID | BSP_KEYBOARD_CAP_RAW | BSP_KEYBOARD_CAP_CHAR;
    s->base.int_io   = BSP_KEYBOARD_INT_EXTERNAL;
    s->base.probe    = sim_probe;
    s->base.set_mode = sim_set_mode;
    s->base.poll     = sim_poll;
    s->base.deinit   = sim_deinit;
    s_kb = s;
    *out_keyboard = &s->base;
    return ESP_OK;
}
