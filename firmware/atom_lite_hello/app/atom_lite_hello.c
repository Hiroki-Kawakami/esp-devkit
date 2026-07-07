/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Minimal ATOM Lite / AtomS3 Lite sample: no display, just the WS2812 RGB LED
 * and the front button. A click cycles the LED through a colour palette; a long
 * press turns it off. Shared across the esp32 (ATOM Lite) and esp32s3 (AtomS3
 * Lite) targets -- the pin map lives in the BSP board, so the app is identical.
 */

#include "atom_lite_hello.h"
#include "esp_log.h"

static const char *TAG = "atom_lite_hello";

static const uint8_t PALETTE[][3] = {
    {0x20, 0x00, 0x00},  /* red     */
    {0x00, 0x20, 0x00},  /* green   */
    {0x00, 0x00, 0x20},  /* blue    */
    {0x20, 0x20, 0x00},  /* yellow  */
    {0x00, 0x20, 0x20},  /* cyan    */
    {0x20, 0x00, 0x20},  /* magenta */
};
#define PALETTE_COUNT (sizeof(PALETTE) / sizeof(PALETTE[0]))

static void on_click(uint8_t id, void *arg) {
    (void)id; (void)arg;
    static size_t idx = 0;
    const uint8_t *c = PALETTE[idx % PALETTE_COUNT];
    bsp_led_set_rgb(0, c[0], c[1], c[2]);
    ESP_LOGI(TAG, "click -> #%02x%02x%02x", c[0], c[1], c[2]);
    idx++;
}

static void on_long_press(uint8_t id, void *arg) {
    (void)id; (void)arg;
    bsp_led_clear();
    ESP_LOGI(TAG, "long press -> off");
}

void app_entry(void) {
    bsp_init(NULL);

    if (bsp_led_count() == 0)    ESP_LOGW(TAG, "no LED provider");
    if (bsp_button_count() == 0) ESP_LOGW(TAG, "no button provider");

    bsp_button_on_click(0, on_click, NULL);
    bsp_button_on_long_press(0, 800, on_long_press, NULL);

    on_click(0, NULL);  /* light the first colour so the LED shows on boot */
}
