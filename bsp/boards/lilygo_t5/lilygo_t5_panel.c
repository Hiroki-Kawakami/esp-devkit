/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * LilyGo T5 ePaper S3 panel binding. The panel data bus, STH, CKH, and CKV
 * are direct GPIOs; power, STV, MODE, OE, and LE are driven by a 74HCT4094D
 * serial output register.
 */

#include "lilygo_t5_panel.h"
#include "bsp_display.h"
#include "ed047tc1.h"
#include "gt911.h"
#include "driver/gpio.h"
#include "esp_attr.h"
#include "esp_log.h"
#include "esp_rom_sys.h"
#include "soc/gpio_struct.h"

static const char *TAG = "lilygo_t5_panel";

#define T5_CFG_DATA_PIN  GPIO_NUM_13
#define T5_CFG_CLK_PIN   GPIO_NUM_12
#define T5_CFG_STR_PIN   GPIO_NUM_0
#define T5_CKV_PIN       GPIO_NUM_38

#define T5_CFG_LE            (1U << 0)
#define T5_CFG_POWER_DISABLE (1U << 1)
#define T5_CFG_POS_ENABLE    (1U << 2)
#define T5_CFG_NEG_ENABLE    (1U << 3)
#define T5_CFG_STV           (1U << 4)
#define T5_CFG_POWER_ENABLE  (1U << 5)
#define T5_CFG_MODE          (1U << 6)
#define T5_CFG_OE            (1U << 7)

#define T5_TOUCH_PIN_INT  GPIO_NUM_47
#define T5_TOUCH_PIN_RST  GPIO_NUM_NC

typedef struct {
    uint8_t value;
} lilygo_t5_control_t;

static lilygo_t5_control_t s_control;

static inline IRAM_ATTR void fast_gpio_set(int pin, bool high) {
    if (pin < 32) {
        if (high) GPIO.out_w1ts = 1U << pin;
        else      GPIO.out_w1tc = 1U << pin;
    } else {
        if (high) GPIO.out1_w1ts.val = 1U << (pin - 32);
        else      GPIO.out1_w1tc.val = 1U << (pin - 32);
    }
}

/* Shift QP7 first through QP0 last, then latch all eight outputs together. */
static IRAM_ATTR void control_push(lilygo_t5_control_t *control) {
    fast_gpio_set(T5_CFG_STR_PIN, false);
    for (int bit = 7; bit >= 0; bit--) {
        fast_gpio_set(T5_CFG_CLK_PIN, false);
        fast_gpio_set(T5_CFG_DATA_PIN, (control->value & (1U << bit)) != 0);
        fast_gpio_set(T5_CFG_CLK_PIN, true);
    }
    fast_gpio_set(T5_CFG_STR_PIN, true);
}

static inline IRAM_ATTR void control_set(lilygo_t5_control_t *control,
                                         uint8_t mask, bool enabled) {
    if (enabled) control->value |= mask;
    else         control->value &= (uint8_t)~mask;
    control_push(control);
}

static inline IRAM_ATTR void control_latch_row(lilygo_t5_control_t *control) {
    control_set(control, T5_CFG_LE, true);
    control_set(control, T5_CFG_LE, false);
}

static inline void ckv_pulse(uint32_t high_us, uint32_t low_us) {
    fast_gpio_set(T5_CKV_PIN, true);
    if (high_us) esp_rom_delay_us(high_us);
    fast_gpio_set(T5_CKV_PIN, false);
    if (low_us) esp_rom_delay_us(low_us);
}

static esp_err_t control_init(void *ctx) {
    lilygo_t5_control_t *control = ctx;
    const gpio_config_t gpio = {
        .pin_bit_mask = (1ULL << T5_CFG_DATA_PIN) | (1ULL << T5_CFG_CLK_PIN) |
                        (1ULL << T5_CFG_STR_PIN) | (1ULL << T5_CKV_PIN),
        .mode         = GPIO_MODE_OUTPUT,
        .intr_type    = GPIO_INTR_DISABLE,
    };
    esp_err_t err = gpio_config(&gpio);
    if (err != ESP_OK) return err;

    fast_gpio_set(T5_CFG_DATA_PIN, false);
    fast_gpio_set(T5_CFG_CLK_PIN, false);
    fast_gpio_set(T5_CFG_STR_PIN, false);
    fast_gpio_set(T5_CKV_PIN, false);
    control->value = 0;
    control_push(control);
    return ESP_OK;
}

static void control_power_on(void *ctx) {
    lilygo_t5_control_t *control = ctx;
    control->value = T5_CFG_POWER_ENABLE;
    control_push(control);
    esp_rom_delay_us(100);

    control_set(control, T5_CFG_NEG_ENABLE, true);
    esp_rom_delay_us(500);
    control_set(control, T5_CFG_POS_ENABLE, true);
    esp_rom_delay_us(100);
    control_set(control, T5_CFG_STV, true);
}

static void control_power_off(void *ctx) {
    lilygo_t5_control_t *control = ctx;
    control_set(control, T5_CFG_POS_ENABLE, false);
    esp_rom_delay_us(10);
    control_set(control, T5_CFG_NEG_ENABLE, false);
    esp_rom_delay_us(100);
    control_set(control, T5_CFG_POWER_DISABLE, true);

    /* PWR_EN low removes the board-side EPD rail. Leave every 4094 output in a
     * known inactive state so the next power_on starts from a clean baseline. */
    control->value = 0;
    control_push(control);
}

static void control_frame_begin(void *ctx) {
    lilygo_t5_control_t *control = ctx;
    control_set(control, T5_CFG_MODE, true);
    ckv_pulse(1, 1);

    control_set(control, T5_CFG_STV, false);
    esp_rom_delay_us(1);

    /* STV must return high while CKV is still high. The LilyGo reference
     * starts this CKV pulse asynchronously and shifts STV high during it; a
     * synchronous ckv_pulse() here would leave STV low through both CKV edges
     * and the gate driver would not see the frame-start pulse correctly. */
    fast_gpio_set(T5_CKV_PIN, true);
    control_set(control, T5_CFG_STV, true);
    esp_rom_delay_us(10);
    fast_gpio_set(T5_CKV_PIN, false);
    esp_rom_delay_us(10);
    ckv_pulse(10, 0);

    control_set(control, T5_CFG_OE, true);
    ckv_pulse(1, 1);
}

static void control_frame_end(void *ctx) {
    lilygo_t5_control_t *control = ctx;

    /* Line transfers are pipelined on this board: each line_begin latches the
     * data shifted by the previous DMA transfer. Latch and gate the final DMA
     * line before disabling the panel output. */
    control_latch_row(control);
    ckv_pulse(1, 1);

    control_set(control, T5_CFG_OE, false);
    control_set(control, T5_CFG_MODE, false);
    ckv_pulse(1, 1);
    ckv_pulse(1, 1);
}

static void control_line_begin(void *ctx) {
    lilygo_t5_control_t *control = ctx;

    /* The 4094-backed LE belongs before the CKV gate pulse and the following
     * DMA transfer. This latches the source data loaded by the previous line. */
    control_latch_row(control);
    fast_gpio_set(T5_CKV_PIN, true);
}

static IRAM_ATTR void control_line_latch(void *ctx) {
    (void)ctx;
    fast_gpio_set(T5_CKV_PIN, false);
}

static esp_err_t touch_init(i2c_master_bus_handle_t bus) {
    const gt911_config_t config = {
        .i2c_bus     = bus,
        .i2c_address = GT911_I2C_ADDR_AUTO,
        .clock_hz    = GT911_I2C_DEFAULT_HZ,
        .int_io      = T5_TOUCH_PIN_INT,
        .reset_io    = T5_TOUCH_PIN_RST,
        .swap_xy     = true,
        .mirror_x    = false,
        .mirror_y    = true,
        .width       = 960,
        .height      = 540,
        .hotknot     = GT911_HOTKNOT_TUNING_DEFAULTS,
    };

    bsp_touch_t *touch = NULL;
    esp_err_t err = gt911_touch_create(&config, &touch);
    if (err != ESP_OK) return err;
    bsp_touch_set_active(touch);
    return ESP_OK;
}

esp_err_t lilygo_t5_panel_init(const bsp_config_t *config,
                               i2c_master_bus_handle_t i2c_bus) {
    const ed047tc1_config_t panel = {
        .data_pins    = { 8, 1, 2, 3, 4, 5, 6, 7 },
        .sph_pin      = 40,
        .cl_pin       = 41,
        .dc_dummy_pin = 45,
        .control = {
            .type = EPD_LL_CONTROL_CUSTOM,
            .custom = {
                .ops = {
                    .init        = control_init,
                    .power_on    = control_power_on,
                    .power_off   = control_power_off,
                    .frame_begin = control_frame_begin,
                    .frame_end   = control_frame_end,
                    .line_begin  = control_line_begin,
                    .line_latch  = control_line_latch,
                },
                .ctx = &s_control,
            },
        },
        .task_priority = config->epd.task_priority ? config->epd.task_priority : 5,
        .task_affinity = config->epd.task_affinity,
    };

    bsp_display_t *display = NULL;
    esp_err_t err = ed047tc1_epd_create(&panel, &display);
    if (err != ESP_OK) return err;
    bsp_display_set_active(display);

    if (i2c_bus) {
        err = touch_init(i2c_bus);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "touch unavailable: %s", esp_err_to_name(err));
        }
    }
    return ESP_OK;
}
