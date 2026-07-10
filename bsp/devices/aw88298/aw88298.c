/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "aw88298.h"
#include <string.h>
#include <stdlib.h>
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "driver/i2s_std.h"

static const char *TAG = "AW88298";

#define AW88298_DEFAULT_ADDR   0x36   /* 7-bit */
#define AW88298_I2C_HZ         400000
#define AW88298_I2C_TIMEOUT_MS 100

#define REG_SYSCTRL   0x04
#define REG_SYSCTRL2  0x05
#define REG_I2SCTRL   0x06
#define REG_HAGCCFG4  0x0C   /* [15:8]=VOL, [7:0]=HOLDTH */
#define REG_BSTCTRL2  0x61

#define SYSCTRL_AMPPD   (1 << 1)
#define SYSCTRL_I2SEN   (1 << 6)
#define SYSCTRL_ENABLE  0x4040   /* I2SEN=1 AMPPD=0 PWDN=0 (bit14 kept set) */
#define SYSCTRL_DISABLE 0x4000   /* I2SEN=0 */

#define HAGCCFG4_HOLDTH 0x0064   /* lower byte; VOL sits in the upper byte */

struct aw88298_state {
    i2c_master_dev_handle_t i2c;
    i2s_chan_handle_t tx;
    uint16_t sysctrl;   /* shadow of reg 0x04 (tracks I2SEN + AMPPD) */
    bool enabled;
    int  volume;
    bool mute;
};

/* 16-bit registers are transmitted MSB-first. */
static esp_err_t reg_write(aw88298_t s, uint8_t reg, uint16_t val) {
    uint8_t buf[3] = { reg, (uint8_t)(val >> 8), (uint8_t)(val & 0xFF) };
    return i2c_master_transmit(s->i2c, buf, sizeof(buf), AW88298_I2C_TIMEOUT_MS);
}

/* VOL field: attenuation encoded as 6 dB/coarse (high nibble) + 0.5 dB/fine
 * (low nibble); 0x00 = 0 dB. Map a linear 0..100 to 0..-96 dB like the codec
 * boards do. */
static uint16_t volume_to_reg(int vol) {
    if (vol >= 100) return HAGCCFG4_HOLDTH;
    if (vol < 0) vol = 0;
    int atten = (100 - vol) * 96 / 100;
    int hi = atten / 6;
    if (hi > 0xF) hi = 0xF;
    int lo = (atten - hi * 6) * 2;
    uint8_t vol_byte = (uint8_t)((hi << 4) | (lo & 0xF));
    return (uint16_t)((vol_byte << 8) | HAGCCFG4_HOLDTH);
}

static i2s_data_bit_width_t to_bit_width(uint8_t bits) {
    switch (bits) {
        case 24: return I2S_DATA_BIT_WIDTH_24BIT;
        case 32: return I2S_DATA_BIT_WIDTH_32BIT;
        case 16:
        default: return I2S_DATA_BIT_WIDTH_16BIT;
    }
}

/* I2SCTRL sample-rate code (M5 CoreS3 table): index of the first entry >= the
 * scaled rate, OR'd with the fixed BCK/format bits. */
static uint16_t rate_to_i2sctrl(uint32_t sample_rate) {
    static const uint8_t rate_tbl[] = { 4, 5, 6, 8, 10, 11, 15, 20, 22, 44 };
    size_t idx = 0;
    size_t rate = (sample_rate + 1102) / 2205;
    while (rate > rate_tbl[idx] && ++idx < sizeof(rate_tbl)) {}
    return (uint16_t)(idx | 0x14C0);
}

static esp_err_t apply_format(aw88298_t s, uint32_t rate, uint8_t bits, uint8_t ch) {
    if (s->enabled) {
        esp_err_t err = i2s_channel_disable(s->tx);
        if (err != ESP_OK) return err;
        s->enabled = false;
    }
    i2s_std_clk_config_t clk = I2S_STD_CLK_DEFAULT_CONFIG(rate);
    i2s_std_slot_config_t slot = I2S_STD_PHILIPS_SLOT_DEFAULT_CONFIG(
        to_bit_width(bits), ch == 1 ? I2S_SLOT_MODE_MONO : I2S_SLOT_MODE_STEREO);
    esp_err_t err = i2s_channel_reconfig_std_clock(s->tx, &clk);
    if (err == ESP_OK) err = i2s_channel_reconfig_std_slot(s->tx, &slot);
    if (err != ESP_OK) return err;
    if ((err = i2s_channel_enable(s->tx)) != ESP_OK) return err;
    s->enabled = true;
    return ESP_OK;
}

esp_err_t aw88298_init(const aw88298_config_t *config, aw88298_t *out) {
    if (!config || !out || !config->i2c_bus) return ESP_ERR_INVALID_ARG;

    aw88298_t s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;
    s->volume = 100;
    s->sysctrl = SYSCTRL_DISABLE | SYSCTRL_AMPPD;   /* silent until open() */

    esp_err_t ret;
    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = config->i2c_address ? config->i2c_address : AW88298_DEFAULT_ADDR,
        .scl_speed_hz    = AW88298_I2C_HZ,
    };
    ret = i2c_master_bus_add_device(config->i2c_bus, &dev_cfg, &s->i2c);
    if (ret != ESP_OK) { ESP_LOGE(TAG, "i2c add device: %d", ret); goto err_free; }

    i2s_chan_config_t chan_cfg = I2S_CHANNEL_DEFAULT_CONFIG(config->i2s_port, I2S_ROLE_MASTER);
    chan_cfg.auto_clear = true;
    ret = i2s_new_channel(&chan_cfg, &s->tx, NULL);
    if (ret != ESP_OK) { ESP_LOGE(TAG, "i2s_new_channel: %d", ret); goto err_dev; }

    const i2s_std_config_t std_cfg = {
        .clk_cfg  = I2S_STD_CLK_DEFAULT_CONFIG(48000),
        .slot_cfg = I2S_STD_PHILIPS_SLOT_DEFAULT_CONFIG(I2S_DATA_BIT_WIDTH_16BIT, I2S_SLOT_MODE_STEREO),
        .gpio_cfg = {
            .mclk = I2S_GPIO_UNUSED,
            .bclk = config->bclk_gpio,
            .ws   = config->ws_gpio,
            .dout = config->dout_gpio,
            .din  = I2S_GPIO_UNUSED,
            .invert_flags = { 0 },
        },
    };
    ret = i2s_channel_init_std_mode(s->tx, &std_cfg);
    if (ret != ESP_OK) { ESP_LOGE(TAG, "i2s init_std: %d", ret); goto err_chan; }

    ret = reg_write(s, REG_SYSCTRL, s->sysctrl);   /* leave amp disabled */
    if (ret != ESP_OK) { ESP_LOGE(TAG, "sysctrl init failed"); goto err_chan; }

    *out = s;
    return ESP_OK;

err_chan:
    i2s_del_channel(s->tx);
err_dev:
    i2c_master_bus_rm_device(s->i2c);
err_free:
    free(s);
    return ret;
}

esp_err_t aw88298_deinit(aw88298_t s) {
    if (!s) return ESP_ERR_INVALID_ARG;
    aw88298_close(s);
    if (s->tx)  i2s_del_channel(s->tx);
    if (s->i2c) i2c_master_bus_rm_device(s->i2c);
    free(s);
    return ESP_OK;
}

esp_err_t aw88298_open(aw88298_t s, uint32_t rate, uint8_t bits, uint8_t ch) {
    if (!s) return ESP_ERR_INVALID_ARG;
    esp_err_t err = apply_format(s, rate, bits, ch);
    if (err != ESP_OK) return err;
    err |= reg_write(s, REG_BSTCTRL2, 0x0673);   /* boost mode disabled */
    err |= reg_write(s, REG_I2SCTRL, rate_to_i2sctrl(rate));
    err |= reg_write(s, REG_SYSCTRL2, 0x0008);   /* RMSE/HAGCE/HDCCE/HMUTE=0 */
    err |= reg_write(s, REG_HAGCCFG4, volume_to_reg(s->volume));
    s->sysctrl = SYSCTRL_ENABLE | SYSCTRL_AMPPD;
    err |= reg_write(s, REG_SYSCTRL, s->sysctrl);
    err |= aw88298_set_mute(s, s->mute);
    return err ? ESP_FAIL : ESP_OK;
}

esp_err_t aw88298_close(aw88298_t s) {
    if (!s) return ESP_ERR_INVALID_ARG;
    if (!s->enabled) return ESP_OK;
    esp_err_t err = aw88298_set_mute(s, true);
    s->sysctrl = SYSCTRL_DISABLE | SYSCTRL_AMPPD;
    err |= reg_write(s, REG_SYSCTRL, s->sysctrl);
    err |= i2s_channel_disable(s->tx);
    s->enabled = false;
    return err ? ESP_FAIL : ESP_OK;
}

esp_err_t aw88298_write(aw88298_t s, const void *data, size_t len) {
    if (!s || !data) return ESP_ERR_INVALID_ARG;
    if (!s->enabled) return ESP_ERR_INVALID_STATE;
    size_t written = 0;
    return i2s_channel_write(s->tx, data, len, &written, portMAX_DELAY);
}

esp_err_t aw88298_set_volume(aw88298_t s, int volume) {
    if (!s) return ESP_ERR_INVALID_ARG;
    if (volume < 0) volume = 0;
    if (volume > 100) volume = 100;
    s->volume = volume;
    if (!s->enabled) return ESP_OK;
    return reg_write(s, REG_HAGCCFG4, volume_to_reg(volume));
}

esp_err_t aw88298_set_mute(aw88298_t s, bool mute) {
    if (!s) return ESP_ERR_INVALID_ARG;
    s->mute = mute;
    s->sysctrl = (s->sysctrl & ~SYSCTRL_AMPPD) | (mute ? SYSCTRL_AMPPD : 0);
    return reg_write(s, REG_SYSCTRL, s->sysctrl);
}

int aw88298_get_volume(aw88298_t s) {
    return s ? s->volume : -1;
}
