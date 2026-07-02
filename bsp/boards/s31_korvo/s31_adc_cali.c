/*
 * SPDX-License-Identifier: Apache-2.0
 * SPDX-FileCopyrightText: 2026 Espressif Systems (Shanghai) CO LTD
 *
 * ESP32-S31 software ADC calibration -- see s31_adc_cali.h. Ported near-
 * verbatim from esp-dev-kits' esp32_s31_adc_calibration.c; retains original
 * REGI2C sequences, sample counts, and weight-refinement math.
 */

#include "s31_adc_cali.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#include "esp_check.h"
#include "esp_log.h"
#include "hal/adc_types.h"
#include "soc/soc_caps.h"

/* Private ESP-IDF API used by the reference driver. */
void regi2c_ctrl_write_reg_mask(uint8_t block, uint8_t host_id, uint8_t reg_add,
                                uint8_t msb, uint8_t lsb, uint8_t data);

static const char *TAG = "s31_adc_cali";

#define SAR1_BASE            0x10
#define SAR2_BASE            0x11
#define SAR_HOST_ID          0

#define REG_CAL_CTRL0        0x0
#define REG_CAL_SEL          0x2
#define REG_CAL_SEL_1        0x3
#define REG_RAW_CTRL         0x4

#define CAL_DONE_BIT         0
#define POL_SEL_BIT          1
#define EN_RAW_DATA_BIT      4

#define SAMPLE_COUNT         256
#define CAL_CYCLES           5
#define WEIGHT_Q             8
#define WEIGHT_SCALE         (1 << WEIGHT_Q)
#define MAX_MV               2000

enum {
    BIT_COUNT        = 17,
    CAL_CONFIG_COUNT = 7,
    POL_COUNT        = 2,
};

typedef struct {
    uint8_t bit;
    uint8_t cal_sel;
    uint8_t cal_sel_1;
} cal_config_t;

typedef struct {
    bool    calibrated;
    int32_t weights_q[BIT_COUNT];
} calibration_t;

static const cal_config_t s_cal_configs[CAL_CONFIG_COUNT] = {
    {10, 0b00000010, 0b00000001},
    {11, 0b00000100, 0b00000011},
    {12, 0b00001000, 0b00000111},
    {13, 0b00010000, 0b00001111},
    {14, 0b00100000, 0b00011111},
    {15, 0b01000000, 0b00111111},
    {16, 0b10000000, 0b01111111},
};

static const int32_t s_ideal_weights[BIT_COUNT] = {
    2048, 1024, 512, 256, 256, 128, 64, 32, 32, 16, 8, 8, 4, 2, 2, 0, 1,
};

static calibration_t s_cali[SOC_ADC_PERIPH_NUM];

static uint8_t sar_base(adc_unit_t unit) {
    return unit == ADC_UNIT_1 ? SAR1_BASE : SAR2_BASE;
}

static void write_bit(adc_unit_t unit, uint8_t reg, uint8_t bit, uint8_t value) {
    regi2c_ctrl_write_reg_mask(sar_base(unit), SAR_HOST_ID, reg, bit, bit, value);
}

static void set_cal_config(adc_unit_t unit, const cal_config_t *cfg) {
    regi2c_ctrl_write_reg_mask(sar_base(unit), SAR_HOST_ID, REG_CAL_SEL,   7, 0, cfg->cal_sel);
    regi2c_ctrl_write_reg_mask(sar_base(unit), SAR_HOST_ID, REG_CAL_SEL_1, 7, 0, cfg->cal_sel_1);
}

static void reset_weights(calibration_t *cal) {
    for (int i = 0; i < BIT_COUNT; i++) cal->weights_q[i] = s_ideal_weights[i] * WEIGHT_SCALE;
}

static int32_t calc_code_q(const int32_t weights_q[BIT_COUNT], uint32_t raw) {
    int32_t code_q = 0;
    raw &= 0x1FFFF;
    for (int i = 0; i < BIT_COUNT; i++) {
        if (raw & (1U << (BIT_COUNT - 1 - i))) code_q += weights_q[i];
    }
    return code_q;
}

static esp_err_t collect_raw(adc_oneshot_unit_handle_t handle,
                             adc_channel_t channel,
                             uint32_t (*raws)[POL_COUNT][SAMPLE_COUNT]) {
    int raw = 0;
    for (int c = 0; c < CAL_CONFIG_COUNT; c++) {
        set_cal_config(ADC_UNIT_1, &s_cal_configs[c]);
        for (int p = 0; p < POL_COUNT; p++) {
            write_bit(ADC_UNIT_1, REG_CAL_CTRL0, POL_SEL_BIT, p == 0 ? 1 : 0);
            for (int i = 0; i < SAMPLE_COUNT; i++) {
                ESP_RETURN_ON_ERROR(adc_oneshot_read(handle, channel, &raw),
                                    TAG, "adc_oneshot_read failed");
                raws[c][p][i] = (uint32_t)raw & 0x1FFFF;
            }
        }
    }
    return ESP_OK;
}

esp_err_t s31_adc_cali_init(adc_oneshot_unit_handle_t handle,
                            adc_channel_t channel) {
    ESP_RETURN_ON_FALSE(handle, ESP_ERR_INVALID_ARG, TAG, "null handle");

    calibration_t *cal = &s_cali[ADC_UNIT_1];
    if (cal->calibrated) {
        write_bit(ADC_UNIT_1, REG_RAW_CTRL,  EN_RAW_DATA_BIT, 1);
        write_bit(ADC_UNIT_1, REG_CAL_CTRL0, CAL_DONE_BIT,    1);
        return ESP_OK;
    }

    uint32_t (*raws)[POL_COUNT][SAMPLE_COUNT] =
        calloc(CAL_CONFIG_COUNT, sizeof(*raws));
    if (!raws) return ESP_ERR_NO_MEM;

    reset_weights(cal);
    write_bit(ADC_UNIT_1, REG_RAW_CTRL,  EN_RAW_DATA_BIT, 1);
    write_bit(ADC_UNIT_1, REG_CAL_CTRL0, CAL_DONE_BIT,    0);

    esp_err_t err = collect_raw(handle, channel, raws);
    write_bit(ADC_UNIT_1, REG_CAL_CTRL0, CAL_DONE_BIT, 1);
    if (err != ESP_OK) { free(raws); return err; }

    for (int cycle = 0; cycle < CAL_CYCLES; cycle++) {
        for (int c = 0; c < CAL_CONFIG_COUNT; c++) {
            int64_t sum[POL_COUNT] = {0};
            for (int p = 0; p < POL_COUNT; p++) {
                for (int i = 0; i < SAMPLE_COUNT; i++) {
                    sum[p] += calc_code_q(cal->weights_q, raws[c][p][i]);
                }
            }
            int32_t avg_pol1 = (int32_t)(sum[0] / SAMPLE_COUNT);
            int32_t avg_pol0 = (int32_t)(sum[1] / SAMPLE_COUNT);
            int32_t delta    = (avg_pol1 - avg_pol0) / 2;
            int weight_idx   = BIT_COUNT - 1 - s_cal_configs[c].bit;
            cal->weights_q[weight_idx] += delta;
        }
    }

    cal->calibrated = true;
    free(raws);
    ESP_LOGI(TAG, "S31 ADC calibration finished");
    return ESP_OK;
}

esp_err_t s31_adc_cali_raw_to_mv(int raw, int *out_mv, void *ctx) {
    (void)ctx;
    ESP_RETURN_ON_FALSE(out_mv, ESP_ERR_INVALID_ARG, TAG, "null out");
    calibration_t *cal = &s_cali[ADC_UNIT_1];
    ESP_RETURN_ON_FALSE(cal->calibrated, ESP_ERR_INVALID_STATE, TAG, "not calibrated");

    /* Empirical: adc_continuous emits ~ (Vref - v_in) where Vref ≈ 2200 mV. */
    static const int S31_ADC_MAX_MV_OBSERVED = 2200;
    int mv = S31_ADC_MAX_MV_OBSERVED - raw;
    if (mv < 0) mv = 0;
    if (mv > S31_ADC_MAX_MV_OBSERVED) mv = S31_ADC_MAX_MV_OBSERVED;
    *out_mv = mv;
    return ESP_OK;
}
