/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "es8388.h"
#include <string.h>
#include <stdlib.h>
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "driver/i2s_std.h"

static const char *TAG = "ES8388";

#define ES8388_DEFAULT_ADDR   0x10   /* 7-bit */
#define ES8388_I2C_HZ         100000
#define ES8388_I2C_TIMEOUT_MS 100

#define REG_CONTROL1     0x00
#define REG_CONTROL2     0x01
#define REG_CHIPPOWER    0x02
#define REG_ADCPOWER     0x03
#define REG_DACPOWER     0x04
#define REG_MASTERMODE   0x08
#define REG_DACCONTROL1  0x17
#define REG_DACCONTROL2  0x18
#define REG_DACCONTROL3  0x19
#define REG_DACCONTROL4  0x1a
#define REG_DACCONTROL5  0x1b
#define REG_DACCONTROL16 0x26
#define REG_DACCONTROL17 0x27
#define REG_DACCONTROL20 0x2a
#define REG_DACCONTROL21 0x2b
#define REG_DACCONTROL23 0x2d
#define REG_DACCONTROL24 0x2e
#define REG_DACCONTROL25 0x2f
#define REG_DACCONTROL26 0x30
#define REG_DACCONTROL27 0x31

#define DAC_MUTE_BIT     0x04    /* DACCONTROL3 bit2 */

struct es8388_state {
    i2c_master_dev_handle_t i2c;
    i2s_chan_handle_t tx;
    uint8_t dac_outputs;
    bool enabled;
    uint32_t rate;
    uint8_t  bits;
    uint8_t  channels;
    int volume;
    bool mute;
};

static esp_err_t reg_write(es8388_t s, uint8_t reg, uint8_t val) {
    uint8_t buf[2] = { reg, val };
    return i2c_master_transmit(s->i2c, buf, sizeof(buf), ES8388_I2C_TIMEOUT_MS);
}

static esp_err_t reg_read(es8388_t s, uint8_t reg, uint8_t *val) {
    return i2c_master_transmit_receive(s->i2c, &reg, 1, val, 1, ES8388_I2C_TIMEOUT_MS);
}

/* ES8388 DAC digital-volume field: 0x00 = 0 dB, 0xC0 = -96 dB (0.5 dB/step). */
static uint8_t volume_to_reg(int vol) {
    if (vol <= 0)   return 0xC0;
    if (vol >= 100) return 0x00;
    return (uint8_t)(((100 - vol) * 192 + 50) / 100);
}

static uint8_t bits_to_word_len(uint8_t bits) {
    switch (bits) {
        case 24: return 0x00;
        case 20: return 0x01;
        case 18: return 0x02;
        case 32: return 0x04;
        case 16:
        default: return 0x03;
    }
}

static i2s_data_bit_width_t to_bit_width(uint8_t bits) {
    switch (bits) {
        case 24: return I2S_DATA_BIT_WIDTH_24BIT;
        case 32: return I2S_DATA_BIT_WIDTH_32BIT;
        case 16:
        default: return I2S_DATA_BIT_WIDTH_16BIT;
    }
}

/* DAC register bring-up; ends powered down + muted (silent until es8388_open). */
static esp_err_t codec_reset(es8388_t s) {
    esp_err_t r = ESP_OK;
    r |= reg_write(s, REG_DACCONTROL3, DAC_MUTE_BIT);   /* mute */
    r |= reg_write(s, REG_CONTROL2,    0x50);
    r |= reg_write(s, REG_CHIPPOWER,   0x00);           /* power up analog/DLL */
    /* Disable the internal DLL to improve low sample rates (per reference). */
    r |= reg_write(s, 0x35, 0xA0);
    r |= reg_write(s, 0x37, 0xD0);
    r |= reg_write(s, 0x39, 0xD0);
    r |= reg_write(s, REG_MASTERMODE,  0x00);           /* I2S slave */
    r |= reg_write(s, REG_DACPOWER,    0xC0);           /* DAC + all outputs off */
    r |= reg_write(s, REG_CONTROL1,    0x12);           /* play mode */
    r |= reg_write(s, REG_DACCONTROL1, 0x18);           /* 16-bit I2S */
    r |= reg_write(s, REG_DACCONTROL2, 0x02);           /* 256fs */
    r |= reg_write(s, REG_DACCONTROL16, 0x00);
    r |= reg_write(s, REG_DACCONTROL17, 0x90);          /* left DAC -> left mixer, 0 dB */
    r |= reg_write(s, REG_DACCONTROL20, 0x90);          /* right DAC -> right mixer, 0 dB */
    r |= reg_write(s, REG_DACCONTROL21, 0x80);          /* common LRCK, enable DAC path */
    r |= reg_write(s, REG_DACCONTROL23, 0x00);          /* vroi = 0 */
    r |= reg_write(s, REG_DACCONTROL4, 0x00);           /* LDAC 0 dB */
    r |= reg_write(s, REG_DACCONTROL5, 0x00);           /* RDAC 0 dB */
    r |= reg_write(s, REG_DACCONTROL24, 0x1E);          /* LOUT1 0 dB */
    r |= reg_write(s, REG_DACCONTROL25, 0x1E);          /* ROUT1 0 dB */
    r |= reg_write(s, REG_DACCONTROL26, 0x1E);          /* LOUT2 0 dB */
    r |= reg_write(s, REG_DACCONTROL27, 0x1E);          /* ROUT2 0 dB */
    r |= reg_write(s, REG_ADCPOWER,    0xFF);           /* ADC unused: powered down */
    return r ? ESP_FAIL : ESP_OK;
}

static esp_err_t apply_format(es8388_t s, uint32_t rate, uint8_t bits, uint8_t ch) {
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
    err = reg_write(s, REG_DACCONTROL1, (uint8_t)(bits_to_word_len(bits) << 3));
    if (err != ESP_OK) return err;
    err = i2s_channel_enable(s->tx);
    if (err != ESP_OK) return err;
    s->enabled = true;
    s->rate = rate; s->bits = bits; s->channels = ch;
    return ESP_OK;
}

esp_err_t es8388_init(const es8388_config_t *config, es8388_t *out) {
    if (!config || !out || !config->i2c_bus) return ESP_ERR_INVALID_ARG;

    es8388_t s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;
    s->volume = -1;
    s->dac_outputs = config->dac_outputs ? config->dac_outputs
                                         : (ES8388_OUT_LOUT1 | ES8388_OUT_ROUT1);

    esp_err_t ret;
    const i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address  = config->i2c_address ? config->i2c_address : ES8388_DEFAULT_ADDR,
        .scl_speed_hz    = ES8388_I2C_HZ,
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
            .mclk = config->mclk_gpio,
            .bclk = config->bclk_gpio,
            .ws   = config->ws_gpio,
            .dout = config->dout_gpio,
            .din  = config->din_gpio,
            .invert_flags = { 0 },
        },
    };
    ret = i2s_channel_init_std_mode(s->tx, &std_cfg);
    if (ret != ESP_OK) { ESP_LOGE(TAG, "i2s init_std: %d", ret); goto err_chan; }

    ret = codec_reset(s);
    if (ret != ESP_OK) { ESP_LOGE(TAG, "codec reset failed"); goto err_chan; }

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

esp_err_t es8388_deinit(es8388_t s) {
    if (!s) return ESP_ERR_INVALID_ARG;
    es8388_close(s);
    if (s->tx)  i2s_del_channel(s->tx);
    if (s->i2c) i2c_master_bus_rm_device(s->i2c);
    free(s);
    return ESP_OK;
}

esp_err_t es8388_open(es8388_t s, uint32_t rate, uint8_t bits, uint8_t ch) {
    if (!s) return ESP_ERR_INVALID_ARG;
    esp_err_t err = apply_format(s, rate, bits, ch);
    if (err != ESP_OK) return err;
    err = reg_write(s, REG_DACPOWER, s->dac_outputs);
    if (err != ESP_OK) return err;
    return es8388_set_mute(s, s->mute);
}

esp_err_t es8388_close(es8388_t s) {
    if (!s) return ESP_ERR_INVALID_ARG;
    if (!s->enabled) return ESP_OK;
    esp_err_t err = es8388_set_mute(s, true);
    err |= reg_write(s, REG_DACPOWER, 0x00);
    err |= i2s_channel_disable(s->tx);
    s->enabled = false;
    return err ? ESP_FAIL : ESP_OK;
}

esp_err_t es8388_write(es8388_t s, const void *data, size_t len) {
    if (!s || !data) return ESP_ERR_INVALID_ARG;
    if (!s->enabled) return ESP_ERR_INVALID_STATE;
    size_t written = 0;
    return i2s_channel_write(s->tx, data, len, &written, portMAX_DELAY);
}

esp_err_t es8388_set_volume(es8388_t s, int volume) {
    if (!s) return ESP_ERR_INVALID_ARG;
    if (volume < 0) volume = 0;
    if (volume > 100) volume = 100;
    s->volume = volume;
    if (volume == 0) return es8388_set_mute(s, true);
    esp_err_t err = es8388_set_mute(s, s->mute);
    uint8_t reg = volume_to_reg(volume);
    err |= reg_write(s, REG_DACCONTROL4, reg);
    err |= reg_write(s, REG_DACCONTROL5, reg);
    return err ? ESP_FAIL : ESP_OK;
}

esp_err_t es8388_set_mute(es8388_t s, bool mute) {
    if (!s) return ESP_ERR_INVALID_ARG;
    s->mute = mute;
    uint8_t reg = 0;
    esp_err_t err = reg_read(s, REG_DACCONTROL3, &reg);
    if (err != ESP_OK) return err;
    reg = (reg & ~DAC_MUTE_BIT) | (mute ? DAC_MUTE_BIT : 0);
    return reg_write(s, REG_DACCONTROL3, reg);
}

int es8388_get_volume(es8388_t s) {
    return s ? s->volume : -1;
}
