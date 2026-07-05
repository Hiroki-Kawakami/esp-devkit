/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * See adc_battery.h. Reads are averaged raw then calibrated to mV; the level is
 * a linear clamp between the board's empty/full endpoints.
 */

#include "adc_battery.h"
#include "esp_adc/adc_cali.h"
#include "esp_adc/adc_cali_scheme.h"
#include "esp_log.h"
#include <stdlib.h>

static const char *TAG = "adc_battery";

typedef struct {
    bsp_power_t base;
    adc_oneshot_unit_handle_t adc;
    adc_cali_handle_t cali;
    adc_channel_t channel;
    adc_atten_t   atten;
    uint32_t divider_mul, divider_div, empty_mv, full_mv;
    uint8_t  samples;
    gpio_num_t vbus_gpio;
    bool vbus_active_high;
} adc_battery_t;

static esp_err_t cali_create(adc_unit_t unit, adc_channel_t chan, adc_atten_t atten,
                             adc_cali_handle_t *out) {
#if ADC_CALI_SCHEME_CURVE_FITTING_SUPPORTED
    const adc_cali_curve_fitting_config_t cfg = {
        .unit_id = unit, .chan = chan, .atten = atten, .bitwidth = ADC_BITWIDTH_DEFAULT,
    };
    return adc_cali_create_scheme_curve_fitting(&cfg, out);
#elif ADC_CALI_SCHEME_LINE_FITTING_SUPPORTED
    (void)chan;
    const adc_cali_line_fitting_config_t cfg = {
        .unit_id = unit, .atten = atten, .bitwidth = ADC_BITWIDTH_DEFAULT,
    };
    return adc_cali_create_scheme_line_fitting(&cfg, out);
#else
    (void)unit; (void)chan; (void)atten; (void)out;
    return ESP_ERR_NOT_SUPPORTED;
#endif
}

static esp_err_t read_voltage(adc_battery_t *bat, uint32_t *out_mv) {
    int64_t sum = 0;
    for (uint8_t i = 0; i < bat->samples; i++) {
        int raw = 0;
        esp_err_t err = adc_oneshot_read(bat->adc, bat->channel, &raw);
        if (err != ESP_OK) return err;
        sum += raw;
    }
    int mv = 0;
    esp_err_t err = adc_cali_raw_to_voltage(bat->cali, (int)(sum / bat->samples), &mv);
    if (err != ESP_OK) return err;
    *out_mv = (uint32_t)mv * bat->divider_mul / bat->divider_div;
    return ESP_OK;
}

static esp_err_t bat_get_voltage(bsp_power_t *self, uint32_t *out_mv) {
    return read_voltage((adc_battery_t *)self, out_mv);
}

static esp_err_t bat_get_level(bsp_power_t *self, uint8_t *out_percent) {
    adc_battery_t *bat = (adc_battery_t *)self;
    uint32_t mv = 0;
    esp_err_t err = read_voltage(bat, &mv);
    if (err != ESP_OK) return err;
    if (mv <= bat->empty_mv) *out_percent = 0;
    else if (mv >= bat->full_mv) *out_percent = 100;
    else *out_percent = (uint8_t)((mv - bat->empty_mv) * 100 / (bat->full_mv - bat->empty_mv));
    return ESP_OK;
}

static bool bat_vbus_present(bsp_power_t *self) {
    adc_battery_t *bat = (adc_battery_t *)self;
    return gpio_get_level(bat->vbus_gpio) == (bat->vbus_active_high ? 1 : 0);
}

static esp_err_t bat_deinit(bsp_power_t *self) {
    adc_battery_t *bat = (adc_battery_t *)self;
#if ADC_CALI_SCHEME_CURVE_FITTING_SUPPORTED
    adc_cali_delete_scheme_curve_fitting(bat->cali);
#elif ADC_CALI_SCHEME_LINE_FITTING_SUPPORTED
    adc_cali_delete_scheme_line_fitting(bat->cali);
#endif
    adc_oneshot_del_unit(bat->adc);
    free(bat);
    return ESP_OK;
}

esp_err_t adc_battery_create(const adc_battery_config_t *config, bsp_power_t **out_power) {
    if (!config || !out_power || config->divider_div == 0 || config->full_mv <= config->empty_mv) {
        return ESP_ERR_INVALID_ARG;
    }

    adc_battery_t *bat = calloc(1, sizeof(*bat));
    if (!bat) return ESP_ERR_NO_MEM;
    bat->channel          = config->adc_channel;
    bat->atten            = config->adc_atten;
    bat->divider_mul      = config->divider_mul;
    bat->divider_div      = config->divider_div;
    bat->empty_mv         = config->empty_mv;
    bat->full_mv          = config->full_mv;
    bat->samples          = config->samples ? config->samples : 16;
    bat->vbus_gpio        = config->vbus_gpio;
    bat->vbus_active_high = config->vbus_active_high;

    const adc_oneshot_unit_init_cfg_t unit_cfg = { .unit_id = config->adc_unit };
    esp_err_t err = adc_oneshot_new_unit(&unit_cfg, &bat->adc);
    if (err != ESP_OK) { ESP_LOGE(TAG, "adc unit: %s", esp_err_to_name(err)); goto fail_unit; }

    const adc_oneshot_chan_cfg_t chan_cfg = {
        .atten = config->adc_atten, .bitwidth = ADC_BITWIDTH_DEFAULT,
    };
    err = adc_oneshot_config_channel(bat->adc, config->adc_channel, &chan_cfg);
    if (err != ESP_OK) { ESP_LOGE(TAG, "adc channel: %s", esp_err_to_name(err)); goto fail_chan; }

    err = cali_create(config->adc_unit, config->adc_channel, config->adc_atten, &bat->cali);
    if (err != ESP_OK) { ESP_LOGE(TAG, "adc cali: %s", esp_err_to_name(err)); goto fail_chan; }

    bat->base.caps                = BSP_POWER_CAP_BATTERY;
    bat->base.get_battery_voltage = bat_get_voltage;
    bat->base.get_battery_level   = bat_get_level;
    bat->base.deinit              = bat_deinit;

    if (config->vbus_gpio != GPIO_NUM_NC) {
        const gpio_config_t in = {
            .pin_bit_mask = 1ULL << config->vbus_gpio,
            .mode         = GPIO_MODE_INPUT,
        };
        if (gpio_config(&in) == ESP_OK) {
            bat->base.caps        |= BSP_POWER_CAP_VBUS;
            bat->base.vbus_present = bat_vbus_present;
        } else {
            ESP_LOGW(TAG, "vbus gpio config failed; VBUS disabled");
        }
    }

    *out_power = &bat->base;
    return ESP_OK;

fail_chan:
    adc_oneshot_del_unit(bat->adc);
fail_unit:
    free(bat);
    return err;
}
