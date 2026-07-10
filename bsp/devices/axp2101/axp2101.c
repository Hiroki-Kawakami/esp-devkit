/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "axp2101.h"
#include <stdlib.h>
#include "esp_log.h"

static const char *TAG = "axp2101";

#define I2C_TIMEOUT_MS  100

/* Registers. */
#define REG_STATUS1     0x00   /* bit5: VBUS good; bit3: battery connected */
#define REG_COMMON_CFG  0x10   /* bit0: software power off */
#define REG_ADC_EN      0x30   /* bit0: battery-voltage ADC enable */
#define REG_BAT_DET     0x68   /* bit0: battery-presence detection enable */
#define REG_VBAT_H      0x34   /* [5:0] high; 14-bit total, 1 mV/LSB */
#define REG_VBAT_L      0x35   /* [7:0] low (read 0x34..0x35 as one burst) */
#define REG_DCDC_EN     0x80   /* bit0..4: DCDC1..DCDC5 enable */
#define REG_DCDC1_VOLT  0x82   /* per-DCDC voltage regs 0x82..0x86 */
#define REG_LDO_EN0     0x90   /* ALDO1..4 = bit0..3, BLDO1/2 = bit4/5, DLDO1 = bit7 */
#define REG_ALDO1_VOLT  0x92   /* ALDO1..4 0x92..0x95, BLDO1/2 0x96/0x97, DLDO1 0x99 */

struct axp2101_dev {
    i2c_master_dev_handle_t i2c;
};

esp_err_t axp2101_write_reg(axp2101_handle_t h, uint8_t reg, uint8_t val) {
    uint8_t buf[2] = { reg, val };
    return i2c_master_transmit(h->i2c, buf, sizeof(buf), I2C_TIMEOUT_MS);
}

esp_err_t axp2101_read_reg(axp2101_handle_t h, uint8_t reg, uint8_t *val) {
    return i2c_master_transmit_receive(h->i2c, &reg, 1, val, 1, I2C_TIMEOUT_MS);
}

static esp_err_t update_reg(axp2101_handle_t h, uint8_t reg, uint8_t clear, uint8_t set) {
    uint8_t v;
    esp_err_t err = axp2101_read_reg(h, reg, &v);
    if (err != ESP_OK) return err;
    v = (uint8_t)((v & ~clear) | set);
    return axp2101_write_reg(h, reg, v);
}

static int clampi(int v, int lo, int hi) { return v < lo ? lo : v > hi ? hi : v; }

/* Enable-bit location for each rail. */
static void rail_enable_bit(axp2101_rail_t rail, uint8_t *reg, uint8_t *bit) {
    if (rail <= AXP2101_DCDC5) { *reg = REG_DCDC_EN; *bit = (uint8_t)(1u << rail); return; }
    *reg = REG_LDO_EN0;
    switch (rail) {
    case AXP2101_ALDO1: *bit = 1u << 0; break;
    case AXP2101_ALDO2: *bit = 1u << 1; break;
    case AXP2101_ALDO3: *bit = 1u << 2; break;
    case AXP2101_ALDO4: *bit = 1u << 3; break;
    case AXP2101_BLDO1: *bit = 1u << 4; break;
    case AXP2101_BLDO2: *bit = 1u << 5; break;
    case AXP2101_DLDO1: *bit = 1u << 7; break;
    default:            *bit = 0;       break;
    }
}

/* Voltage register for the LDO rails (ALDO/BLDO/DLDO); DCDC uses 0x82+index. */
static uint8_t rail_volt_reg(axp2101_rail_t rail) {
    switch (rail) {
    case AXP2101_ALDO1: return REG_ALDO1_VOLT + 0;
    case AXP2101_ALDO2: return REG_ALDO1_VOLT + 1;
    case AXP2101_ALDO3: return REG_ALDO1_VOLT + 2;
    case AXP2101_ALDO4: return REG_ALDO1_VOLT + 3;
    case AXP2101_BLDO1: return REG_ALDO1_VOLT + 4;   /* 0x96 */
    case AXP2101_BLDO2: return REG_ALDO1_VOLT + 5;   /* 0x97 */
    case AXP2101_DLDO1: return REG_ALDO1_VOLT + 7;   /* 0x99 */
    default:            return (uint8_t)(REG_DCDC1_VOLT + rail);
    }
}

esp_err_t axp2101_set_rail_mv(axp2101_handle_t h, axp2101_rail_t rail, int mv) {
    if (rail <= AXP2101_DCDC5) return ESP_ERR_NOT_SUPPORTED;   /* LCD path uses LDOs only */
    uint8_t n = (uint8_t)((clampi(mv, 500, 3500) - 500) / 100);   /* 0.5V + n*0.1V */
    return update_reg(h, rail_volt_reg(rail), 0x1F, n);
}

esp_err_t axp2101_set_rail_enabled(axp2101_handle_t h, axp2101_rail_t rail, bool on) {
    uint8_t reg, bit;
    rail_enable_bit(rail, &reg, &bit);
    if (!bit) return ESP_ERR_INVALID_ARG;
    return update_reg(h, reg, on ? 0 : bit, on ? bit : 0);
}

bool axp2101_battery_present(axp2101_handle_t h) {
    uint8_t v;
    return axp2101_read_reg(h, REG_STATUS1, &v) == ESP_OK && (v & 0x08);
}

esp_err_t axp2101_get_battery_voltage_mv(axp2101_handle_t h, uint32_t *out_mv) {
    /* With no battery the vbat ADC input floats and reads garbage (drifting /
     * occasional full-scale), so gate on the connect bit first. */
    if (!axp2101_battery_present(h)) { *out_mv = 0; return ESP_OK; }

    /* Read the 14-bit result as one auto-incrementing burst; two separate reads
     * can tear as the ADC updates between them. */
    uint8_t reg = REG_VBAT_H, d[2];
    esp_err_t err = i2c_master_transmit_receive(h->i2c, &reg, 1, d, sizeof(d), I2C_TIMEOUT_MS);
    if (err != ESP_OK) return err;
    *out_mv = (uint32_t)(((d[0] & 0x3F) << 8) | d[1]);   /* 1 mV/LSB */
    return ESP_OK;
}

bool axp2101_vbus_present(axp2101_handle_t h) {
    uint8_t v;
    return axp2101_read_reg(h, REG_STATUS1, &v) == ESP_OK && (v & 0x20);
}

esp_err_t axp2101_power_off(axp2101_handle_t h) {
    return update_reg(h, REG_COMMON_CFG, 0, 0x01);
}

esp_err_t axp2101_create(const axp2101_config_t *cfg, axp2101_handle_t *out_handle) {
    if (!cfg || !cfg->i2c_bus || !out_handle) return ESP_ERR_INVALID_ARG;

    axp2101_handle_t h = calloc(1, sizeof(*h));
    if (!h) return ESP_ERR_NO_MEM;

    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = cfg->i2c_address ? cfg->i2c_address : AXP2101_I2C_ADDR,
        .scl_speed_hz    = cfg->clock_hz ? cfg->clock_hz : AXP2101_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(cfg->i2c_bus, &dev_cfg, &h->i2c);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "i2c_master_bus_add_device: %s", esp_err_to_name(err));
        free(h);
        return err;
    }

    uint8_t id;
    if ((err = axp2101_read_reg(h, REG_STATUS1, &id)) != ESP_OK) {
        ESP_LOGE(TAG, "not responding on 0x%02x", dev_cfg.device_address);
        i2c_master_bus_rm_device(h->i2c);
        free(h);
        return err;
    }
    update_reg(h, REG_ADC_EN,  0, 0x01);   /* battery-voltage ADC on */
    update_reg(h, REG_BAT_DET, 0, 0x01);   /* battery-presence detection on */
    /* rails are left to the caller */

    *out_handle = h;
    ESP_LOGI(TAG, "AXP2101 ready");
    return ESP_OK;
}

esp_err_t axp2101_destroy(axp2101_handle_t h) {
    if (!h) return ESP_ERR_INVALID_ARG;
    if (h->i2c) i2c_master_bus_rm_device(h->i2c);
    free(h);
    return ESP_OK;
}

/* --- bsp_power sensing provider --------------------------------------------- */

typedef struct {
    bsp_power_t      base;
    axp2101_handle_t axp;
    uint32_t         empty_mv;
    uint32_t         full_mv;
} axp2101_power_t;

static esp_err_t power_get_voltage(bsp_power_t *self, uint32_t *out_mv) {
    return axp2101_get_battery_voltage_mv(((axp2101_power_t *)self)->axp, out_mv);
}

static esp_err_t power_get_level(bsp_power_t *self, uint8_t *out_percent) {
    axp2101_power_t *p = (axp2101_power_t *)self;
    uint32_t mv;
    esp_err_t err = axp2101_get_battery_voltage_mv(p->axp, &mv);
    if (err != ESP_OK) return err;
    if (mv <= p->empty_mv) { *out_percent = 0; return ESP_OK; }
    if (mv >= p->full_mv)  { *out_percent = 100; return ESP_OK; }
    *out_percent = (uint8_t)((mv - p->empty_mv) * 100 / (p->full_mv - p->empty_mv));
    return ESP_OK;
}

static bool power_vbus_present(bsp_power_t *self) {
    return axp2101_vbus_present(((axp2101_power_t *)self)->axp);
}

static esp_err_t power_deinit(bsp_power_t *self) { free(self); return ESP_OK; }

esp_err_t axp2101_power_create(axp2101_handle_t h, uint32_t empty_mv, uint32_t full_mv,
                               bsp_power_t **out_power) {
    if (!h || !out_power || full_mv <= empty_mv) return ESP_ERR_INVALID_ARG;
    axp2101_power_t *p = calloc(1, sizeof(*p));
    if (!p) return ESP_ERR_NO_MEM;
    p->axp      = h;
    p->empty_mv = empty_mv;
    p->full_mv  = full_mv;
    p->base.caps = BSP_POWER_CAP_BATTERY | BSP_POWER_CAP_VBUS;
    p->base.get_battery_voltage = power_get_voltage;
    p->base.get_battery_level   = power_get_level;
    p->base.vbus_present        = power_vbus_present;
    p->base.deinit              = power_deinit;
    *out_power = &p->base;
    return ESP_OK;
}
