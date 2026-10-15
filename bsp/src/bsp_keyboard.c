/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Common keyboard layer: connection tracking, INT-driven draining of the active
 * provider's event queue and delivery on the shared dispatch task.
 */

#include "bsp.h"
#include "bsp_keyboard.h"
#include "bsp_dispatch.h"
#include "bsp_harness.h"
#include "sdkconfig.h"

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

#ifdef ESP_PLATFORM
#include "driver/gpio.h"
#endif

static const char *TAG = "bsp_keyboard";

#define POLL_INTERVAL_MS    20
#define DETECT_INTERVAL_MS  1000
#define RECHECK_INTERVAL_MS 100
#define DISCONNECT_MISSES   3
#define DRAIN_CHUNK         8

static bsp_keyboard_t        *s_kb;
static bsp_keyboard_mode_t    s_mode;
static bsp_keyboard_detect_t  s_detect;
static SemaphoreHandle_t      s_lock;
static volatile bool          s_connected;
static bool                   s_reported;
static uint8_t                s_misses;

static bsp_keyboard_event_cb_t s_event_cb;
static void                   *s_event_arg;

static uint32_t keyboard_tick(void *ctx);
static bsp_dispatch_source_t s_source = { .tick = keyboard_tick };

static inline void lock(void)   { xSemaphoreTake(s_lock, portMAX_DELAY); }
static inline void unlock(void) { xSemaphoreGive(s_lock); }

static void emit(const bsp_keyboard_event_t *event) {
    bsp_keyboard_event_cb_t cb = s_event_cb;
    if (cb) cb(event, s_event_arg);
}

/* Caller holds s_lock. */
static bool connect_locked(void) {
    if (s_kb->probe(s_kb) != ESP_OK) {
        s_connected = false;
        return false;
    }
    if (!s_connected) {
        esp_err_t err = s_kb->set_mode(s_kb, s_mode);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "set_mode on connect: %s", esp_err_to_name(err));
            return false;
        }
        s_connected = true;
    }
    s_misses = 0;
    return true;
}

/* Caller holds s_lock. A bus glitch must not look like a detach, so AUTO
 * re-probes a few times before giving up. */
static void miss_locked(void) {
    if (s_detect != BSP_KEYBOARD_DETECT_AUTO || ++s_misses >= DISCONNECT_MISSES) {
        s_connected = false;
        s_misses    = 0;
    }
}

/* Caller holds s_lock. A polled provider's own polls notice it is gone. */
static void check_locked(void) {
    if (s_detect != BSP_KEYBOARD_DETECT_AUTO) return;
    if (!s_connected) {
        connect_locked();
        return;
    }
    if (s_misses == 0 && s_kb->int_io == -1) return;
    if (s_kb->probe(s_kb) == ESP_OK) {
        s_misses = 0;
    } else {
        miss_locked();
    }
}

static void report_connection(void) {
    bool connected = s_connected;
    if (connected == s_reported) return;
    s_reported = connected;
    emit(&(bsp_keyboard_event_t){
        .type = connected ? BSP_KEYBOARD_EVENT_CONNECTED : BSP_KEYBOARD_EVENT_DISCONNECTED,
    });
}

static uint32_t keyboard_tick(void *ctx) {
    (void)ctx;
    bsp_keyboard_t *kb = s_kb;
    if (!kb) return BSP_DISPATCH_IDLE;

    lock();
    check_locked();
    bool drain = s_connected && s_misses == 0;
    unlock();
    report_connection();

    while (drain) {
        bsp_keyboard_event_t events[DRAIN_CHUNK];
        uint8_t n = 0;
        lock();
        esp_err_t err = s_connected ? kb->poll(kb, events, DRAIN_CHUNK, &n) : ESP_ERR_INVALID_STATE;
        if (err != ESP_OK && s_connected) {
            ESP_LOGW(TAG, "poll: %s", esp_err_to_name(err));
            miss_locked();
        }
        unlock();
        for (uint8_t i = 0; i < n; i++) emit(&events[i]);
        drain = (err == ESP_OK && n == DRAIN_CHUNK);
    }
    report_connection();

    if (!s_connected) {
        return s_detect == BSP_KEYBOARD_DETECT_AUTO ? DETECT_INTERVAL_MS : BSP_DISPATCH_IDLE;
    }
    if (s_misses > 0) return RECHECK_INTERVAL_MS;
    if (kb->int_io == -1) return POLL_INTERVAL_MS;
    return s_detect == BSP_KEYBOARD_DETECT_AUTO ? DETECT_INTERVAL_MS : BSP_DISPATCH_IDLE;
}

#ifdef ESP_PLATFORM
static void IRAM_ATTR keyboard_int_isr(void *arg) {
    (void)arg;
    BaseType_t hp = pdFALSE;
    bsp_dispatch_notify_from_isr(&s_source, &hp);
    if (hp) portYIELD_FROM_ISR();
}

static esp_err_t attach_isr(int int_io) {
    esp_err_t err = bsp_dispatch_install_gpio_isr();
    if (err != ESP_OK) return err;
    gpio_set_intr_type((gpio_num_t)int_io, GPIO_INTR_NEGEDGE);
    err = gpio_isr_handler_add((gpio_num_t)int_io, keyboard_int_isr, NULL);
    if (err != ESP_OK) return err;
    return gpio_intr_enable((gpio_num_t)int_io);
}
#endif

void bsp_keyboard_set_active(bsp_keyboard_t *keyboard, const bsp_keyboard_init_t *init) {
    if (!keyboard) return;
    if (!(keyboard->caps & BSP_KEYBOARD_CAP_HID)) {
        ESP_LOGE(TAG, "provider without HID support");
        return;
    }
    if (!s_lock) s_lock = xSemaphoreCreateMutex();

    s_mode   = init ? init->mode : BSP_KEYBOARD_MODE_HID;
    s_detect = init ? init->detect : BSP_KEYBOARD_DETECT_MANUAL;
    if (!(keyboard->caps & (1u << s_mode))) {
        ESP_LOGW(TAG, "mode %d not supported, using HID", (int)s_mode);
        s_mode = BSP_KEYBOARD_MODE_HID;
    }
    s_kb = keyboard;

#ifdef ESP_PLATFORM
    if (keyboard->int_io >= 0) {
        esp_err_t err = attach_isr(keyboard->int_io);
        if (err != ESP_OK) ESP_LOGE(TAG, "attach INT: %s", esp_err_to_name(err));
    }
#endif

    lock();
    connect_locked();
    unlock();
    bsp_dispatch_add_source(&s_source);
}

void bsp_keyboard_notify(void) {
    bsp_dispatch_notify(&s_source);
}

uint32_t bsp_keyboard_get_caps(void) {
    return s_kb ? s_kb->caps : 0;
}

bool bsp_keyboard_is_connected(void) {
    return s_connected;
}

esp_err_t bsp_keyboard_probe(void) {
    if (!s_kb) return ESP_ERR_NOT_SUPPORTED;
    lock();
    bool connected = connect_locked();
    unlock();
    bsp_dispatch_notify(&s_source);
    return connected ? ESP_OK : ESP_ERR_NOT_FOUND;
}

esp_err_t bsp_keyboard_set_mode(bsp_keyboard_mode_t mode) {
    if (!s_kb || mode > BSP_KEYBOARD_MODE_CHAR || !(s_kb->caps & (1u << mode))) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    lock();
    esp_err_t err = s_connected ? s_kb->set_mode(s_kb, mode) : ESP_OK;
    if (err == ESP_OK) s_mode = mode;
    unlock();
    return err;
}

bsp_keyboard_mode_t bsp_keyboard_get_mode(void) {
    return s_mode;
}

void bsp_keyboard_set_event_cb(bsp_keyboard_event_cb_t cb, void *arg) {
    s_event_arg = arg;
    s_event_cb  = cb;
}

#if CONFIG_BSP_HARNESS
esp_err_t bsp_harness_keyboard_inject(const bsp_keyboard_event_t *event) {
    if (!s_kb) return ESP_ERR_NOT_SUPPORTED;
    if (!event) return ESP_ERR_INVALID_ARG;
    if (event->type <= BSP_KEYBOARD_EVENT_CHAR && (bsp_keyboard_mode_t)event->type != s_mode) {
        return ESP_ERR_INVALID_STATE;
    }
    emit(event);
    return ESP_OK;
}
#endif
