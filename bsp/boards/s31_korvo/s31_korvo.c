/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP32-S31-Korvo board (ESP32-S31 + 800x480 RGB parallel LCD). Minimum
 * bring-up: hands off to s31_korvo_panel_init for the RGB LCD. I2C bus (SDA=0,
 * SCL=1) and GT1151 touch will be added when touch support lands.
 */

#include "bsp.h"
#include "bsp_led.h"
#include "bsp_button.h"
#include "bsp_dispatch.h"
#include "bsp_sd.h"
#include "esp_system.h"
#include "esp_log.h"
#include "esp_adc/adc_oneshot.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "s31_korvo_panel.h"
#include "s31_adc_cali.h"
#include "adc_button.h"
#include "sd_mmc.h"
#include "ws2812.h"

static const char *TAG = "s31_korvo";

#define S31_I2C_PORT     I2C_NUM_0
#define S31_I2C_PIN_SDA  GPIO_NUM_0
#define S31_I2C_PIN_SCL  GPIO_NUM_1

#define S31_LED_GPIO     GPIO_NUM_37
#define S31_LED_COUNT    1

#define S31_SD_PIN_D0  GPIO_NUM_20
#define S31_SD_PIN_D1  GPIO_NUM_21
#define S31_SD_PIN_D2  GPIO_NUM_22
#define S31_SD_PIN_D3  GPIO_NUM_23
#define S31_SD_PIN_CLK GPIO_NUM_24
#define S31_SD_PIN_CMD GPIO_NUM_25
#define S31_SD_PIN_EN  GPIO_NUM_39

/* ADC1_CH0 (GPIO42) button ladder. Voltages from esp-dev-kits schematic. */
#define S31_BUTTON_COUNT    4
#define S31_BUTTON_ADC_CHAN ADC_CHANNEL_0
static const uint16_t S31_BUTTON_CENTERS_MV[S31_BUTTON_COUNT] = {
    380,   /* VOL-UP   */
    820,   /* VOL-DOWN */
    1340,  /* MODE     */
    1870,  /* SET      */
};
#define S31_BUTTON_IDLE_MV  2200

static esp_err_t i2c_bus_init(i2c_master_bus_handle_t *out_bus) {
    const i2c_master_bus_config_t cfg = {
        .i2c_port          = S31_I2C_PORT,
        .sda_io_num        = S31_I2C_PIN_SDA,
        .scl_io_num        = S31_I2C_PIN_SCL,
        .clk_source        = I2C_CLK_SRC_DEFAULT,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t err = i2c_new_master_bus(&cfg, out_bus);
    if (err != ESP_OK) ESP_LOGE(TAG, "i2c_new_master_bus: %s", esp_err_to_name(err));
    return err;
}

static esp_err_t sd_power_acquire(void *context, sd_pwr_ctrl_handle_t *out_power) {
    (void)context;
    *out_power = NULL;
    const gpio_config_t power_config = {
        .pin_bit_mask = 1ULL << S31_SD_PIN_EN,
        .mode = GPIO_MODE_OUTPUT,
        .pull_up_en = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type = GPIO_INTR_DISABLE,
    };
    esp_err_t err = gpio_config(&power_config);
    if (err != ESP_OK) return err;
    return gpio_set_level(S31_SD_PIN_EN, 1);
}

static esp_err_t sd_power_release(void *context, sd_pwr_ctrl_handle_t power) {
    (void)context;
    (void)power;
    return gpio_set_level(S31_SD_PIN_EN, 0);
}

static esp_err_t sd_init(void) {
    sdmmc_host_t host = SDMMC_HOST_DEFAULT();
    host.slot = SDMMC_HOST_SLOT_0;
    host.max_freq_khz = SDMMC_FREQ_HIGHSPEED;

    sdmmc_slot_config_t slot_config = SDMMC_SLOT_CONFIG_DEFAULT();
    slot_config.width = 4;
    slot_config.clk = S31_SD_PIN_CLK;
    slot_config.cmd = S31_SD_PIN_CMD;
    slot_config.d0 = S31_SD_PIN_D0;
    slot_config.d1 = S31_SD_PIN_D1;
    slot_config.d2 = S31_SD_PIN_D2;
    slot_config.d3 = S31_SD_PIN_D3;

    const sd_mmc_config_t config = {
        .host = host,
        .slot_config = slot_config,
        .allocation_unit_size = 16 * 1024,
        .power_acquire = sd_power_acquire,
        .power_release = sd_power_release,
    };
    bsp_sd_t *sd = NULL;
    esp_err_t err = sd_mmc_create(&config, &sd);
    if (err != ESP_OK) return err;
    bsp_sd_set_active(sd);
    return ESP_OK;
}

esp_err_t bsp_init(const bsp_config_t *config) {
    bsp_dispatch_configure(config ? config->dispatch.task_priority : 0,
                           config ? config->dispatch.task_affinity : -1);

    bsp_config_t defaults = {0};
    if (!config) config = &defaults;

    i2c_master_bus_handle_t i2c_bus = NULL;
    esp_err_t err = i2c_bus_init(&i2c_bus);
    if (err != ESP_OK) return err;

    /* WS2812 status LED is optional -- log and continue on failure so a bad
     * RMT bring-up doesn't take the display + touch path down with it. */
    const ws2812_config_t led_cfg = {
        .gpio  = S31_LED_GPIO,
        .count = S31_LED_COUNT,
    };
    bsp_led_t *led = NULL;
    if (ws2812_create(&led_cfg, &led) == ESP_OK) {
        bsp_led_set_active(led);
    } else {
        ESP_LOGW(TAG, "ws2812 unavailable");
    }

    esp_err_t panel_err = s31_korvo_panel_init(config, i2c_bus);

    if ((err = sd_init()) != ESP_OK) {
        ESP_LOGW(TAG, "sd unavailable: %s", esp_err_to_name(err));
    }

    /* Button ADC ladder: oneshot only for the S31 cali sweep, then hand ADC1
     * over to adc_continuous (owned by adc_button). Any failure leaves the
     * ladder unavailable without breaking the display path. */
    adc_oneshot_unit_handle_t cali_adc = NULL;
    const adc_oneshot_unit_init_cfg_t unit_cfg = { .unit_id = ADC_UNIT_1 };
    if (adc_oneshot_new_unit(&unit_cfg, &cali_adc) == ESP_OK) {
        const adc_oneshot_chan_cfg_t chan_cfg = {
            .atten    = ADC_ATTEN_DB_0,
            .bitwidth = ADC_BITWIDTH_DEFAULT,
        };
        esp_err_t e = adc_oneshot_config_channel(cali_adc, S31_BUTTON_ADC_CHAN, &chan_cfg);
        if (e == ESP_OK) e = s31_adc_cali_init(cali_adc, S31_BUTTON_ADC_CHAN);
        adc_oneshot_del_unit(cali_adc);

        if (e == ESP_OK) {
            const adc_button_config_t btn_cfg = {
                .adc_channel   = S31_BUTTON_ADC_CHAN,
                .raw_to_mv     = s31_adc_cali_raw_to_mv,
                .centers_mv    = S31_BUTTON_CENTERS_MV,
                .idle_mv       = S31_BUTTON_IDLE_MV,
                .count         = S31_BUTTON_COUNT,
            };
            bsp_button_raw_t *btn = NULL;
            if (adc_button_create(&btn_cfg, &btn) == ESP_OK) {
                bsp_button_add_raw(btn);
            } else {
                ESP_LOGW(TAG, "adc_button_create failed");
            }
        } else {
            ESP_LOGW(TAG, "ADC cali failed: %s", esp_err_to_name(e));
        }
    } else {
        ESP_LOGW(TAG, "adc_oneshot_new_unit failed");
    }

    return panel_err;
}
