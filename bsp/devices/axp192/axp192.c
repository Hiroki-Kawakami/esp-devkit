/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "axp192.h"
#include <stdlib.h>
#include "esp_log.h"

static const char *TAG = "axp192";

#define I2C_TIMEOUT_MS  100

/* Registers. */
#define REG_POWER_STATUS  0x00   /* bit5: VBUS present */
#define REG_OUTPUT_CTRL   0x12   /* DC-DC / LDO enable, bits match AXP192_RAIL_* */
#define REG_DCDC2_VOLT    0x23   /* [5:0] */
#define REG_DCDC1_VOLT    0x26   /* [6:0] */
#define REG_DCDC3_VOLT    0x27   /* [6:0] */
#define REG_LDO23_VOLT    0x28   /* [7:4] LDO2, [3:0] LDO3 */
#define REG_GPIO0_FUNC    0x90   /* [2:0] GPIO0 mode: 0x02 = LDOio0 on, 0x07 = float */
#define REG_GPIO0_VOLT    0x91   /* [7:4] LDOio0 voltage */
#define REG_ADC_EN1       0x82
#define REG_CHARGE_CTRL   0x33
#define REG_BACKUP_CHARGE 0x35
#define REG_IRQ_EN3       0x42   /* [1] PEK short, [0] PEK long press enable */
#define REG_IRQ_STATUS3   0x46   /* same layout; write 1 to clear */
#define REG_PEK           0x36   /* [5:4] long-press time: 00=1.0 01=1.5 10=2.0 11=2.5 s */
#define REG_SHUTDOWN      0x32   /* bit7: power off */
#define REG_BATT_VOLT_H   0x78   /* 8 MSB */
#define REG_BATT_VOLT_L   0x79   /* 4 LSB */

struct axp192_dev {
    i2c_master_dev_handle_t i2c;
};

esp_err_t axp192_write_reg(axp192_handle_t h, uint8_t reg, uint8_t val) {
    uint8_t buf[2] = { reg, val };
    return i2c_master_transmit(h->i2c, buf, sizeof(buf), I2C_TIMEOUT_MS);
}

esp_err_t axp192_read_reg(axp192_handle_t h, uint8_t reg, uint8_t *val) {
    return i2c_master_transmit_receive(h->i2c, &reg, 1, val, 1, I2C_TIMEOUT_MS);
}

static esp_err_t update_reg(axp192_handle_t h, uint8_t reg, uint8_t clear, uint8_t set) {
    uint8_t v;
    esp_err_t err = axp192_read_reg(h, reg, &v);
    if (err != ESP_OK) return err;
    v = (v & ~clear) | set;
    return axp192_write_reg(h, reg, v);
}

static int clampi(int v, int lo, int hi) { return v < lo ? lo : v > hi ? hi : v; }

esp_err_t axp192_set_ldo2_mv(axp192_handle_t h, int mv) {
    uint8_t n = (uint8_t)((clampi(mv, 1800, 3300) - 1800) / 100);   /* 1.8V + n*0.1V */
    return update_reg(h, REG_LDO23_VOLT, 0xF0, (uint8_t)(n << 4));
}

esp_err_t axp192_set_ldo3_mv(axp192_handle_t h, int mv) {
    uint8_t n = (uint8_t)((clampi(mv, 1800, 3300) - 1800) / 100);
    return update_reg(h, REG_LDO23_VOLT, 0x0F, n);
}

esp_err_t axp192_set_dcdc1_mv(axp192_handle_t h, int mv) {
    uint8_t n = (uint8_t)((clampi(mv, 700, 3500) - 700) / 25);      /* 0.7V + n*0.025V */
    return update_reg(h, REG_DCDC1_VOLT, 0x7F, n);
}

esp_err_t axp192_set_dcdc2_mv(axp192_handle_t h, int mv) {
    uint8_t n = (uint8_t)((clampi(mv, 700, 2275) - 700) / 25);
    return update_reg(h, REG_DCDC2_VOLT, 0x3F, n);
}

esp_err_t axp192_set_dcdc3_mv(axp192_handle_t h, int mv) {
    uint8_t n = (uint8_t)((clampi(mv, 700, 3500) - 700) / 25);
    return update_reg(h, REG_DCDC3_VOLT, 0x7F, n);
}

esp_err_t axp192_set_rail_enabled(axp192_handle_t h, uint32_t rail_mask, bool on) {
    uint8_t m = (uint8_t)(rail_mask & 0x5F);
    return update_reg(h, REG_OUTPUT_CTRL, on ? 0 : m, on ? m : 0);
}

esp_err_t axp192_set_ldoio0_mv(axp192_handle_t h, int mv) {
    uint8_t n = (uint8_t)((clampi(mv, 1800, 3300) - 1800) / 100);
    return update_reg(h, REG_GPIO0_VOLT, 0xF0, (uint8_t)(n << 4));
}

esp_err_t axp192_set_ldoio0_enabled(axp192_handle_t h, bool on) {
    return update_reg(h, REG_GPIO0_FUNC, 0x07, on ? 0x02 : 0x07);
}

esp_err_t axp192_get_battery_voltage_mv(axp192_handle_t h, uint32_t *out_mv) {
    uint8_t hi, lo;
    esp_err_t err = axp192_read_reg(h, REG_BATT_VOLT_H, &hi);
    if (err != ESP_OK) return err;
    err = axp192_read_reg(h, REG_BATT_VOLT_L, &lo);
    if (err != ESP_OK) return err;
    uint32_t raw = ((uint32_t)hi << 4) | (lo & 0x0F);   /* 12-bit, 1.1 mV/LSB */
    *out_mv = raw * 11 / 10;
    return ESP_OK;
}

bool axp192_vbus_present(axp192_handle_t h) {
    uint8_t v;
    return axp192_read_reg(h, REG_POWER_STATUS, &v) == ESP_OK && (v & 0x20);
}

esp_err_t axp192_poll_power_key(axp192_handle_t h, bool *short_press, bool *long_press) {
    uint8_t st;
    esp_err_t err = axp192_read_reg(h, REG_IRQ_STATUS3, &st);
    if (err != ESP_OK) return err;
    uint8_t pek = st & 0x03;
    if (pek) axp192_write_reg(h, REG_IRQ_STATUS3, pek);
    if (short_press) *short_press = pek & 0x02;
    if (long_press)  *long_press  = pek & 0x01;
    return ESP_OK;
}

esp_err_t axp192_set_pek_long_ms(axp192_handle_t h, uint16_t ms) {
    uint8_t field = ms <= 1000 ? 0 : ms <= 1500 ? 1 : ms <= 2000 ? 2 : 3;   /* ceil */
    return update_reg(h, REG_PEK, 0x30, (uint8_t)(field << 4));
}

esp_err_t axp192_power_off(axp192_handle_t h) {
    return update_reg(h, REG_SHUTDOWN, 0, 0x80);
}

esp_err_t axp192_create(const axp192_config_t *cfg, axp192_handle_t *out_handle) {
    if (!cfg || !cfg->i2c_bus || !out_handle) return ESP_ERR_INVALID_ARG;

    axp192_handle_t h = calloc(1, sizeof(*h));
    if (!h) return ESP_ERR_NO_MEM;

    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = cfg->i2c_address ? cfg->i2c_address : AXP192_I2C_ADDR,
        .scl_speed_hz    = cfg->clock_hz ? cfg->clock_hz : AXP192_I2C_DEFAULT_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(cfg->i2c_bus, &dev_cfg, &h->i2c);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "i2c_master_bus_add_device: %s", esp_err_to_name(err));
        free(h);
        return err;
    }

    /* Probe + chip-wide defaults: enable all ADCs and the battery charger
     * (4.2 V target). Rails are left to the caller. */
    uint8_t id;
    if ((err = axp192_read_reg(h, REG_POWER_STATUS, &id)) != ESP_OK) {
        ESP_LOGE(TAG, "not responding on 0x%02x", dev_cfg.device_address);
        i2c_master_bus_rm_device(h->i2c);
        free(h);
        return err;
    }
    axp192_write_reg(h, REG_ADC_EN1, 0xFF);        /* battery/VBUS/ACIN/APS/TS ADC on */
    axp192_write_reg(h, REG_CHARGE_CTRL, 0xC0);    /* charge on, 4.2 V, min current */
    axp192_write_reg(h, REG_BACKUP_CHARGE, 0xA2);  /* RTC coin cell: on, 3.0 V */
    update_reg(h, REG_IRQ_EN3, 0, 0x03);           /* latch PEK short/long press events */

    *out_handle = h;
    ESP_LOGI(TAG, "AXP192 ready");
    return ESP_OK;
}

esp_err_t axp192_destroy(axp192_handle_t h) {
    if (!h) return ESP_ERR_INVALID_ARG;
    if (h->i2c) i2c_master_bus_rm_device(h->i2c);
    free(h);
    return ESP_OK;
}

/* --- bsp_power sensing provider --------------------------------------------- */

typedef struct {
    bsp_power_t     base;
    axp192_handle_t axp;
    uint32_t        empty_mv;
    uint32_t        full_mv;
} axp192_power_t;

static esp_err_t power_get_voltage(bsp_power_t *self, uint32_t *out_mv) {
    return axp192_get_battery_voltage_mv(((axp192_power_t *)self)->axp, out_mv);
}

static esp_err_t power_get_level(bsp_power_t *self, uint8_t *out_percent) {
    axp192_power_t *p = (axp192_power_t *)self;
    uint32_t mv;
    esp_err_t err = axp192_get_battery_voltage_mv(p->axp, &mv);
    if (err != ESP_OK) return err;
    if (mv <= p->empty_mv) { *out_percent = 0; return ESP_OK; }
    if (mv >= p->full_mv)  { *out_percent = 100; return ESP_OK; }
    *out_percent = (uint8_t)((mv - p->empty_mv) * 100 / (p->full_mv - p->empty_mv));
    return ESP_OK;
}

static bool power_vbus_present(bsp_power_t *self) {
    return axp192_vbus_present(((axp192_power_t *)self)->axp);
}

static esp_err_t power_deinit(bsp_power_t *self) {
    free(self);
    return ESP_OK;
}

esp_err_t axp192_power_create(axp192_handle_t h, uint32_t empty_mv, uint32_t full_mv,
                              bsp_power_t **out_power) {
    if (!h || !out_power || full_mv <= empty_mv) return ESP_ERR_INVALID_ARG;
    axp192_power_t *p = calloc(1, sizeof(*p));
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
