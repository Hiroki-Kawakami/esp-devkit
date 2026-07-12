/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "i2s_dac.h"
#include <stdlib.h>
#include <math.h>
#include "freertos/FreeRTOS.h"
#include "driver/i2s_std.h"
#include "soc/soc_caps.h"

#if SOC_DAC_SUPPORTED
#include "driver/rtc_io.h"
#include "hal/dac_ll.h"
#include "hal/i2s_ll.h"
#include "hal/i2s_hal.h"
#include "soc/i2s_struct.h"
#include "esp_private/esp_clk.h"
#endif

#define GAIN_Q15        15
#define GAIN_UNITY      (1 << GAIN_Q15)
#define VOLUME_DB_SPAN  40.0f   /* vol=1 -> -40 dB, vol=100 -> 0 dB */
#define DAC_BIAS        0x8000  /* signed 16-bit -> unsigned; DAC reads the top 8 bits */
#define SCRATCH_SAMPLES 128

struct i2s_dac_state {
    i2s_chan_handle_t tx;
    int32_t gain;   /* Q15 sample multiplier from the volume curve */
    int  volume;
    bool mute;
    bool enabled;
    bool internal;      /* route to the ESP32 built-in DAC */
    gpio_num_t dac_gpio;
    uint8_t in_channels;   /* input interleave, for the mono downmix (internal DAC) */
};

static i2s_data_bit_width_t to_bit_width(uint8_t bits) {
    switch (bits) {
        case 24: return I2S_DATA_BIT_WIDTH_24BIT;
        case 32: return I2S_DATA_BIT_WIDTH_32BIT;
        case 16:
        default: return I2S_DATA_BIT_WIDTH_16BIT;
    }
}

static int32_t volume_to_gain(int vol) {
    if (vol <= 0) return 0;
    if (vol >= 100) return GAIN_UNITY;
    float db = (vol - 100) * (VOLUME_DB_SPAN / 100.0f);
    return (int32_t)(powf(10.0f, db / 20.0f) * GAIN_UNITY + 0.5f);
}

static i2s_std_slot_config_t make_slot(i2s_dac_t s, uint8_t bits, i2s_slot_mode_t mode) {
    i2s_std_slot_config_t slot = I2S_STD_PHILIPS_SLOT_DEFAULT_CONFIG(
        to_bit_width(s->internal ? 16 : bits), mode);
#if SOC_DAC_SUPPORTED
    if (s->internal) {
        slot.slot_mask      = I2S_STD_SLOT_BOTH;
        slot.slot_bit_width = I2S_SLOT_BIT_WIDTH_16BIT;
        slot.msb_right      = false;
    }
#endif
    return slot;
}

static i2s_std_clk_config_t make_clk(i2s_dac_t s, uint32_t rate) {
    i2s_std_clk_config_t clk = I2S_STD_CLK_DEFAULT_CONFIG(rate);
#if SOC_DAC_SUPPORTED
    if (s->internal) {
        clk.clk_src       = I2S_CLK_SRC_PLL_160M;
        clk.mclk_multiple = I2S_MCLK_MULTIPLE_128;
    }
#else
    (void)s;
#endif
    return clk;
}

#if SOC_DAC_SUPPORTED
/* Point the DAC pad at the RTC mux, power the channel, and put I2S0 into the
 * built-in-DAC data path. This replicates Espressif's dac_dma_periph_init: the
 * FIFO must be forced into the 16-bit built-in-DAC mode (only the high 8 bits of
 * each 16-bit slot reach the DAC) — without it the DAC latches the raw byte
 * stream and plays square-wave noise. i2s_channel_reconfig resets these bits, so
 * this is re-applied on every open() before enable. */
#define DAC_I2S_BITS  16   /* only the high 8 bits reach the DAC */

static void dac_setup(i2s_dac_t s, uint32_t rate) {
    dac_channel_t ch = (s->dac_gpio == GPIO_NUM_26) ? DAC_CHAN_1 : DAC_CHAN_0;
    rtc_gpio_init(s->dac_gpio);
    rtc_gpio_set_direction(s->dac_gpio, RTC_GPIO_MODE_DISABLED);
    rtc_gpio_pullup_dis(s->dac_gpio);
    rtc_gpio_pulldown_dis(s->dac_gpio);
    dac_ll_power_on(ch);
    dac_ll_rtc_sync_by_adc(false);
    dac_ll_digi_enable_dma(true);

    i2s_ll_enable_builtin_adc_dac(&I2S0, true);
    i2s_ll_tx_set_sample_bit(&I2S0, DAC_I2S_BITS, DAC_I2S_BITS);
    i2s_ll_tx_enable_mono_mode(&I2S0, true);
    i2s_ll_tx_select_std_slot(&I2S0, I2S_STD_SLOT_BOTH, true);
    i2s_ll_tx_enable_msb_shift(&I2S0, false);
    i2s_ll_tx_set_ws_width(&I2S0, DAC_I2S_BITS);
    i2s_ll_tx_enable_msb_right(&I2S0, false);
    i2s_ll_tx_enable_right_first(&I2S0, true);
    i2s_ll_tx_force_enable_fifo_mod(&I2S0, true);

    /* DAC-specific clock (dac_dma.c): WS = rate, bclk = rate * AD_BCK_FACTOR,
     * mclk = bclk * bit_width, from the 2x-APB (D2PLL) source. The normal I2S
     * serial clock mistimes the parallel DAC latch and yields byte-stream noise. */
    uint32_t bclk = rate * I2S_LL_AD_BCK_FACTOR;
    uint32_t mclk = bclk * DAC_I2S_BITS;
    uint32_t sclk = esp_clk_apb_freq() * 2;
    i2s_ll_tx_clk_set_src(&I2S0, I2S_CLK_SRC_DEFAULT);
    hal_utils_clk_div_t mclk_div = {0};
    i2s_hal_calc_mclk_precise_division(sclk, mclk, &mclk_div);
    i2s_ll_tx_set_mclk(&I2S0, &mclk_div);
    i2s_ll_tx_set_bck_div_num(&I2S0, DAC_I2S_BITS);
}

static void dac_teardown(i2s_dac_t s) {
    dac_channel_t ch = (s->dac_gpio == GPIO_NUM_26) ? DAC_CHAN_1 : DAC_CHAN_0;
    dac_ll_digi_enable_dma(false);
    dac_ll_power_down(ch);
    /* Drive the pad low so the idle amp input is grounded rather than holding the
     * last DC (which the board keeps amplifying as noise). Re-armed by open(). */
    rtc_gpio_set_direction(s->dac_gpio, RTC_GPIO_MODE_OUTPUT_ONLY);
    rtc_gpio_set_level(s->dac_gpio, 0);
}
#endif

esp_err_t i2s_dac_create(const i2s_dac_config_t *config, i2s_dac_t *out) {
    if (!config || !out) return ESP_ERR_INVALID_ARG;
    bool internal = (config->i2s_port == I2S_NUM_INTERNAL);
#if !SOC_DAC_SUPPORTED
    if (internal) return ESP_ERR_NOT_SUPPORTED;
#endif
    /* The built-in DAC is wired to the I2S0 DMA on ESP32. */
    int port = internal ? I2S_NUM_0 : config->i2s_port;

    i2s_dac_t s = calloc(1, sizeof(*s));
    if (!s) return ESP_ERR_NO_MEM;
    s->volume = 100;
    s->gain = GAIN_UNITY;
    s->internal = internal;
    s->dac_gpio = internal ? config->dac_gpio : GPIO_NUM_NC;
    s->in_channels = 2;

    i2s_chan_config_t chan_cfg = I2S_CHANNEL_DEFAULT_CONFIG(port, I2S_ROLE_MASTER);
    chan_cfg.auto_clear = true;
    if (internal) {
        chan_cfg.dma_desc_num  = 8;
        chan_cfg.dma_frame_num = 256;
    }
    esp_err_t err = i2s_new_channel(&chan_cfg, &s->tx, NULL);
    if (err != ESP_OK) { free(s); return err; }

    i2s_std_gpio_config_t gpio_cfg = {
        .mclk = I2S_GPIO_UNUSED,
        .din  = I2S_GPIO_UNUSED,
        .invert_flags = { 0 },
    };
    if (internal) {
        gpio_cfg.bclk = I2S_GPIO_UNUSED;
        gpio_cfg.ws   = I2S_GPIO_UNUSED;
        gpio_cfg.dout = config->dac_gpio;
    } else {
        gpio_cfg.bclk = config->bclk_gpio;
        gpio_cfg.ws   = config->ws_gpio;
        gpio_cfg.dout = config->dout_gpio;
    }
    const i2s_std_config_t std_cfg = {
        .clk_cfg  = make_clk(s, 48000),
        .slot_cfg = make_slot(s, 16, internal ? I2S_SLOT_MODE_MONO : I2S_SLOT_MODE_STEREO),
        .gpio_cfg = gpio_cfg,
    };
    err = i2s_channel_init_std_mode(s->tx, &std_cfg);
    if (err != ESP_OK) { i2s_del_channel(s->tx); free(s); return err; }

#if SOC_DAC_SUPPORTED
    if (s->internal) dac_setup(s, 48000);
#endif

    *out = s;
    return ESP_OK;
}

esp_err_t i2s_dac_deinit(i2s_dac_t s) {
    if (!s) return ESP_ERR_INVALID_ARG;
    i2s_dac_close(s);   /* powers the internal DAC down */
    if (s->tx) i2s_del_channel(s->tx);
    free(s);
    return ESP_OK;
}

esp_err_t i2s_dac_open(i2s_dac_t s, uint32_t sample_rate, uint8_t bits_per_sample, uint8_t channels) {
    if (!s) return ESP_ERR_INVALID_ARG;
    if (s->enabled) {
        esp_err_t e = i2s_channel_disable(s->tx);
        if (e != ESP_OK) return e;
        s->enabled = false;
    }
    /* The built-in DAC runs the I2S in 16-bit mono (samples packed 2-per-word in
     * write); the input channel count only drives the downmix. */
    i2s_slot_mode_t slot_mode = (s->internal || channels == 1) ? I2S_SLOT_MODE_MONO
                                                               : I2S_SLOT_MODE_STEREO;
    i2s_std_clk_config_t clk = make_clk(s, sample_rate);
    i2s_std_slot_config_t slot = make_slot(s, bits_per_sample, slot_mode);
    esp_err_t err = i2s_channel_reconfig_std_clock(s->tx, &clk);
    if (err == ESP_OK) err = i2s_channel_reconfig_std_slot(s->tx, &slot);
    if (err != ESP_OK) return err;
#if SOC_DAC_SUPPORTED
    if (s->internal) dac_setup(s, sample_rate);
#endif
    if ((err = i2s_channel_enable(s->tx)) != ESP_OK) return err;
    s->in_channels = channels ? channels : 2;
    s->enabled = true;
    return ESP_OK;
}

esp_err_t i2s_dac_close(i2s_dac_t s) {
    if (!s) return ESP_ERR_INVALID_ARG;
    esp_err_t err = ESP_OK;
    if (s->enabled) {
        err = i2s_channel_disable(s->tx);
        s->enabled = false;
    }
#if SOC_DAC_SUPPORTED
    /* Power the DAC down when idle so the speaker goes quiet between streams. */
    if (s->internal) dac_teardown(s);
#endif
    return err;
}

static esp_err_t write_external(i2s_dac_t s, const void *data, size_t len) {
    size_t written = 0;
    int32_t gain = s->mute ? 0 : s->gain;
    if (gain == GAIN_UNITY) {
        return i2s_channel_write(s->tx, data, len, &written, portMAX_DELAY);
    }
    const int16_t *in = data;
    size_t total = len / sizeof(int16_t);
    int16_t scratch[SCRATCH_SAMPLES];
    for (size_t off = 0; off < total; ) {
        size_t n = total - off;
        if (n > SCRATCH_SAMPLES) n = SCRATCH_SAMPLES;
        for (size_t i = 0; i < n; i++) {
            scratch[i] = (int16_t)(((int32_t)in[off + i] * gain) >> GAIN_Q15);
        }
        esp_err_t err = i2s_channel_write(s->tx, scratch, n * sizeof(int16_t), &written, portMAX_DELAY);
        if (err != ESP_OK) return err;
        off += n;
    }
    return ESP_OK;
}

/* One mono DAC sample from an interleaved input frame: downmix, gain, then bias
 * signed 16-bit to unsigned (the DAC uses the top 8 bits). */
static uint16_t dac_sample(const int16_t *frame, uint8_t ch, int32_t gain) {
    int32_t acc = 0;
    for (uint8_t c = 0; c < ch; c++) acc += frame[c];
    acc = ((acc / ch) * gain) >> GAIN_Q15;
    int32_t u = acc + DAC_BIAS;
    if (u < 0) u = 0;
    else if (u > 0xFFFF) u = 0xFFFF;
    return (uint16_t)u;
}

/* Built-in DAC (mono): one 16-bit sample per frame with the value in the high
 * byte (the DAC reads the high 8 bits; the FIFO is in built-in-DAC mode). */
static esp_err_t write_internal(i2s_dac_t s, const void *data, size_t len) {
    int32_t gain = s->mute ? 0 : s->gain;
    uint8_t ch = s->in_channels ? s->in_channels : 1;
    const int16_t *in = data;
    size_t frames = (len / sizeof(int16_t)) / ch;

    uint16_t scratch[SCRATCH_SAMPLES];
    for (size_t fi = 0; fi < frames; ) {
        size_t n = 0;
        while (n < SCRATCH_SAMPLES && fi < frames) {
            scratch[n++] = dac_sample(&in[fi * ch], ch, gain);
            fi++;
        }
        size_t written = 0;
        esp_err_t err = i2s_channel_write(s->tx, scratch, n * sizeof(uint16_t), &written, portMAX_DELAY);
        if (err != ESP_OK) return err;
    }
    return ESP_OK;
}

esp_err_t i2s_dac_write(i2s_dac_t s, const void *data, size_t len) {
    if (!s || !data) return ESP_ERR_INVALID_ARG;
    if (!s->enabled) return ESP_ERR_INVALID_STATE;
    return s->internal ? write_internal(s, data, len) : write_external(s, data, len);
}

esp_err_t i2s_dac_set_volume(i2s_dac_t s, int volume) {
    if (!s) return ESP_ERR_INVALID_ARG;
    if (volume < 0) volume = 0;
    if (volume > 100) volume = 100;
    s->volume = volume;
    s->gain = volume_to_gain(volume);
    return ESP_OK;
}

esp_err_t i2s_dac_set_mute(i2s_dac_t s, bool mute) {
    if (!s) return ESP_ERR_INVALID_ARG;
    s->mute = mute;
    return ESP_OK;
}
