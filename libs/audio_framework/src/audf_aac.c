/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdlib.h>
#include <string.h>

#include "audf_aac_internal.h"
#include "audf_aac_kernels.h"
#include "audf_alloc.h"

static const uint32_t s_rates[13] = {
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
};

typedef struct {
    int      aot;
    int      sf_index;
    uint32_t rate;
    int      channels;
    int      sbr;
    int      ps;
    uint32_t ext_rate;
} aac_config_t;

static int rate_index(uint32_t rate) {
    static const uint32_t lower[12] = {
        92017, 75132, 55426, 46009, 37566, 27713, 23004, 18783, 13856, 11502, 9391, 0,
    };
    for (int i = 0; i < 12; i++) {
        if (rate >= lower[i]) return i;
    }
    return 11;
}

static void read_rate(aac_bits_t *b, int *index, uint32_t *rate) {
    *index = aac_bits_get(b, 4);
    if (*index == 15) {
        *rate = aac_bits_get(b, 24);
        *index = rate_index(*rate);
    } else {
        *rate = *index < 13 ? s_rates[*index] : 0;
    }
}

static int read_aot(aac_bits_t *b) {
    int aot = aac_bits_get(b, 5);
    return aot == 31 ? 32 + (int)aac_bits_get(b, 6) : aot;
}

static int parse_pce(aac_bits_t *b) {
    aac_bits_get(b, 4 + 2 + 4);
    const int front = aac_bits_get(b, 4);
    const int side = aac_bits_get(b, 4);
    const int back = aac_bits_get(b, 4);
    const int lfe = aac_bits_get(b, 2);
    const int assoc = aac_bits_get(b, 3);
    const int cc = aac_bits_get(b, 4);
    if (aac_bits_bit(b)) aac_bits_get(b, 4);
    if (aac_bits_bit(b)) aac_bits_get(b, 4);
    if (aac_bits_bit(b)) aac_bits_get(b, 3);
    int channels = 0;
    for (int i = 0; i < front + side + back; i++) {
        channels += aac_bits_bit(b) ? 2 : 1;
        aac_bits_get(b, 4);
    }
    b->pos += 4 * (lfe + assoc) + 5 * cc;
    b->pos = (b->pos + 7) & ~7u;
    b->pos += 8 * aac_bits_get(b, 8);
    if (front + side + back != 1 || lfe || cc) return 0;
    return channels;
}

static esp_err_t parse_asc(const uint8_t *asc, size_t len, aac_config_t *cfg) {
    uint8_t buf[64 + AAC_BUF_PAD] = { 0 };
    if (!len || len > 64) return ESP_ERR_INVALID_ARG;
    memcpy(buf, asc, len);
    aac_bits_t b = { buf, 0, (uint32_t)len * 8 };
    cfg->sbr = -1;
    cfg->ps = -1;
    cfg->aot = read_aot(&b);
    read_rate(&b, &cfg->sf_index, &cfg->rate);
    cfg->channels = aac_bits_get(&b, 4);
    if (cfg->aot == 5 || cfg->aot == 29) {
        cfg->sbr = 1;
        if (cfg->aot == 29) cfg->ps = 1;
        int ext_index;
        read_rate(&b, &ext_index, &cfg->ext_rate);
        cfg->aot = read_aot(&b);
    }
    if (cfg->aot != 2) return ESP_ERR_NOT_SUPPORTED;
    if (aac_bits_bit(&b)) return ESP_ERR_NOT_SUPPORTED;
    if (aac_bits_bit(&b)) aac_bits_get(&b, 14);
    if (aac_bits_bit(&b)) return ESP_ERR_NOT_SUPPORTED;
    if (!cfg->channels) cfg->channels = parse_pce(&b);
    if (cfg->sbr < 0 && aac_bits_left(&b) >= 16 && aac_bits_get(&b, 11) == 0x2b7) {
        if (read_aot(&b) == 5) {
            cfg->sbr = aac_bits_bit(&b);
            if (cfg->sbr) {
                int ext_index;
                read_rate(&b, &ext_index, &cfg->ext_rate);
                if (aac_bits_left(&b) >= 12 && aac_bits_get(&b, 11) == 0x548) cfg->ps = aac_bits_bit(&b);
            }
        }
    }
    if (aac_bits_overrun(&b)) return ESP_ERR_INVALID_SIZE;
    return ESP_OK;
}

typedef struct {
    int      profile;
    int      sf_index;
    int      channels;
    uint32_t frame_len;
    uint32_t header_len;
    int      blocks;
} adts_header_t;

static bool parse_adts(const uint8_t *p, size_t len, adts_header_t *h) {
    if (len < 7 || p[0] != 0xFF || (p[1] & 0xF6) != 0xF0) return false;
    h->profile = p[2] >> 6;
    h->sf_index = (p[2] >> 2) & 0xF;
    h->channels = (p[2] & 1) << 2 | p[3] >> 6;
    h->frame_len = (uint32_t)(p[3] & 3) << 11 | (uint32_t)p[4] << 3 | p[5] >> 5;
    h->blocks = p[6] & 3;
    h->header_len = (p[1] & 1) ? 7 : 9;
    return h->frame_len >= h->header_len;
}

size_t audf_aac_adts_frame_len(const uint8_t *data, size_t len) {
    adts_header_t h;
    if (!data || !parse_adts(data, len, &h)) return 0;
    return h.frame_len;
}

static void skip_bits(aac_bits_t *b, uint32_t n) {
    b->pos += n;
}

static esp_err_t parse_cpe(aac_dec_t *d, aac_bits_t *b) {
    aac_ics_t *l = &d->ch[0].ics;
    aac_ics_t *r = &d->ch[1].ics;
    aac_bits_get(b, 4);
    d->common_window = aac_bits_bit(b);
    d->ms_present = 0;
    memset(d->ms_used, 0, sizeof(d->ms_used));
    esp_err_t err;
    if (d->common_window) {
        if ((err = aac_ics_info(d, b, l)) != ESP_OK) return err;
        r->window_sequence = l->window_sequence;
        r->window_shape = l->window_shape;
        r->max_sfb = l->max_sfb;
        r->num_windows = l->num_windows;
        r->num_groups = l->num_groups;
        memcpy(r->group_len, l->group_len, sizeof(r->group_len));
        r->num_swb = l->num_swb;
        r->swb_offset = l->swb_offset;
        d->ms_present = aac_bits_get(b, 2);
        if (d->ms_present == 3) return ESP_ERR_INVALID_RESPONSE;
        for (int g = 0; g < l->num_groups; g++) {
            for (int sfb = 0; sfb < l->max_sfb; sfb++) {
                d->ms_used[g][sfb] = d->ms_present == 2 || (d->ms_present == 1 && aac_bits_bit(b));
            }
        }
    }
    if ((err = aac_ics_parse(d, b, &d->ch[0], d->common_window)) != ESP_OK) return err;
    return aac_ics_parse(d, b, &d->ch[1], d->common_window);
}

static esp_err_t parse_fill(aac_dec_t *d, aac_bits_t *b, int element) {
    uint32_t count = aac_bits_get(b, 4);
    if (count == 15) count += aac_bits_get(b, 8) - 1;
    if (!count) return ESP_OK;
    const uint32_t bits = count * 8;
    const uint32_t start = b->pos;
    const uint32_t type = aac_bits_get(b, 4);
    if ((type == 13 || type == 14) && d->sbr && element >= 0) {
        aac_sbr_parse(d, b, element, bits - 4, type == 14);
    }
    b->pos = start + bits;
    return ESP_OK;
}

static esp_err_t parse_dse(aac_bits_t *b) {
    aac_bits_get(b, 4);
    const bool align = aac_bits_bit(b);
    uint32_t count = aac_bits_get(b, 8);
    if (count == 255) count += aac_bits_get(b, 8);
    if (align) b->pos = (b->pos + 7) & ~7u;
    skip_bits(b, count * 8);
    return ESP_OK;
}

static esp_err_t raw_data_block(aac_dec_t *d, aac_bits_t *b) {
    int element = -1;
    int id;
    esp_err_t err = ESP_OK;
    while ((id = aac_bits_get(b, 3)) != AAC_ID_END) {
        switch (id) {
        case AAC_ID_SCE:
            if (d->core_channels != 1 || element >= 0) return ESP_ERR_NOT_SUPPORTED;
            aac_bits_get(b, 4);
            d->common_window = false;
            memset(d->ms_used, 0, sizeof(d->ms_used));
            err = aac_ics_parse(d, b, &d->ch[0], false);
            element = AAC_ID_SCE;
            break;
        case AAC_ID_CPE:
            if (d->core_channels != 2 || element >= 0) return ESP_ERR_NOT_SUPPORTED;
            err = parse_cpe(d, b);
            element = AAC_ID_CPE;
            break;
        case AAC_ID_FIL:
            err = parse_fill(d, b, element);
            break;
        case AAC_ID_DSE:
            err = parse_dse(b);
            break;
        case AAC_ID_PCE:
            if (parse_pce(b) != d->core_channels) return ESP_ERR_NOT_SUPPORTED;
            break;
        default:
            return ESP_ERR_NOT_SUPPORTED;
        }
        if (err != ESP_OK) return err;
        if (aac_bits_overrun(b)) return ESP_ERR_INVALID_SIZE;
    }
    return element >= 0 ? ESP_OK : ESP_ERR_INVALID_SIZE;
}

static esp_err_t decode(audf_decoder_t *dec, const void *frame, size_t len, void *pcm, size_t *frames) {
    aac_dec_t *d = (aac_dec_t *)dec;
    if (len > AAC_MAX_BYTES) return ESP_ERR_INVALID_SIZE;
    memcpy(d->frame, frame, len);
    memset(d->frame + len, 0, AAC_BUF_PAD);
    aac_bits_t b = { d->frame, 0, (uint32_t)len * 8 };
    if (d->adts) {
        adts_header_t h;
        if (!parse_adts(d->frame, len, &h) || h.frame_len > len) return ESP_ERR_INVALID_SIZE;
        if (h.blocks || h.profile != 1 || h.sf_index != d->sf_index) return ESP_ERR_NOT_SUPPORTED;
        b.pos = h.header_len * 8;
        b.end = h.frame_len * 8;
    }
    aac_k_prepare();
    aac_prof_start(d);
    if (d->sbr) aac_sbr_frame_start(d);
    esp_err_t err = raw_data_block(d, &b);
    if (err != ESP_OK) return err;

    aac_spectrum(d, d->core_channels);
    aac_prof(d, AUDF_AAC_PROF_CORE);
    int16_t *out = pcm;
    if (d->sbr) {
        aac_sbr_apply(d, out);
        *frames = 2 * AAC_FRAME_LEN;
        return ESP_OK;
    }
    for (int ch = 0; ch < d->core_channels; ch++) aac_filterbank(d, &d->ch[ch]);
    aac_prof(d, AUDF_AAC_PROF_FILTERBANK);
    aac_k_pack(out, d->ch[0].time, d->core_channels == 2 ? d->ch[1].time : NULL, AAC_FRAME_LEN, 3);
    *frames = AAC_FRAME_LEN;
    return ESP_OK;
}

static void reset(audf_decoder_t *dec) {
    aac_dec_t *d = (aac_dec_t *)dec;
    for (int ch = 0; ch < d->core_channels; ch++) {
        memset(d->ch[ch].overlap, 0, AAC_FRAME_LEN * sizeof(int32_t));
        d->ch[ch].prev_shape = 0;
    }
    d->noise_seed = 0x1f2e3d4c;
    if (d->sbr) aac_sbr_reset(d);
}

static void destroy(audf_decoder_t *dec) {
    aac_dec_t *d = (aac_dec_t *)dec;
    aac_sbr_destroy(d);
    for (int ch = 0; ch < 2; ch++) {
        audf_free(d->ch[ch].spec);
        audf_free(d->ch[ch].overlap);
        if (!d->sbr_on) audf_free(d->ch[ch].time);
    }
    audf_free(d->scratch);
    audf_free(d->frame);
    audf_free(d);
}

static const audf_decoder_ops_t s_ops = {
    .decode = decode,
    .reset = reset,
    .destroy = destroy,
};

esp_err_t audf_aac_decoder_create(const audf_aac_config_t *config, audf_decoder_t **out) {
    if (!config || !out || !config->asc) return ESP_ERR_INVALID_ARG;
    aac_config_t cfg = { .sbr = -1, .ps = -1 };
    if (config->adts) {
        adts_header_t h;
        if (!parse_adts(config->asc, config->asc_len, &h)) return ESP_ERR_INVALID_ARG;
        if (h.profile != 1) return ESP_ERR_NOT_SUPPORTED;
        cfg.aot = 2;
        cfg.sf_index = h.sf_index;
        cfg.rate = h.sf_index < 13 ? s_rates[h.sf_index] : 0;
        cfg.channels = h.channels;
    } else {
        esp_err_t err = parse_asc(config->asc, config->asc_len, &cfg);
        if (err != ESP_OK) return err;
    }
    if (cfg.sf_index > 12 || !cfg.rate) return ESP_ERR_NOT_SUPPORTED;
    if (cfg.channels < 1 || cfg.channels > 2) return ESP_ERR_NOT_SUPPORTED;

    const uint32_t caps = config->alloc_caps;
    aac_dec_t *d = audf_calloc(1, sizeof(*d), caps);
    if (!d) return ESP_ERR_NO_MEM;
    d->base.ops = &s_ops;
    d->base.fmt = AUDF_FMT_S16;
    d->alloc_caps = caps;
    d->adts = config->adts;
    d->sf_index = cfg.sf_index;
    d->core_rate = cfg.rate;
    d->core_channels = cfg.channels;
    d->noise_seed = 0x1f2e3d4c;
    d->clock = config->clock;

    if (config->he != AUDF_AAC_HE_OFF) {
        if (cfg.sbr == 1) {
            d->sbr_on = !cfg.ext_rate || cfg.ext_rate == 2 * cfg.rate;
        } else {
            d->sbr_on = cfg.sbr < 0 && cfg.rate <= 24000;
        }
    }
    d->ps_on = d->sbr_on && config->he == AUDF_AAC_HE_V2 && cfg.channels == 1 && cfg.ps != 0;
    d->out_rate = d->sbr_on ? 2 * cfg.rate : cfg.rate;
    d->base.channels = d->ps_on ? 2 : cfg.channels;
    d->base.max_frames = d->sbr_on ? 2 * AAC_FRAME_LEN : AAC_FRAME_LEN;

    bool ok = (d->frame = audf_malloc(AAC_MAX_BYTES + AAC_BUF_PAD, caps)) != NULL;
    const size_t scratch_bytes = 3 * AAC_FRAME_LEN * sizeof(int32_t);
    if (config->scratch_caps) d->scratch = audf_malloc_aligned(16, scratch_bytes, config->scratch_caps);
    if (!d->scratch) d->scratch = audf_malloc_aligned(16, scratch_bytes, caps);
    ok = ok && d->scratch != NULL;
    for (int ch = 0; ok && ch < cfg.channels; ch++) {
        ok = (d->ch[ch].spec = audf_malloc_aligned(16, AAC_FRAME_LEN * sizeof(int32_t), caps)) != NULL &&
             (d->ch[ch].overlap = audf_calloc(AAC_FRAME_LEN, sizeof(int32_t), caps)) != NULL &&
             (d->ch[ch].time = d->sbr_on ? d->scratch + AAC_FRAME_LEN
                                         : audf_malloc_aligned(16, AAC_FRAME_LEN * sizeof(int32_t), caps)) != NULL;
        if (ok) memset(d->ch[ch].spec, 0, AAC_FRAME_LEN * sizeof(int32_t));
    }
    if (ok && d->sbr_on) ok = aac_sbr_create(d) == ESP_OK;
    if (!ok) {
        destroy(&d->base);
        return ESP_ERR_NO_MEM;
    }
    *out = &d->base;
    return ESP_OK;
}

bool audf_aac_take_profile(audf_decoder_t *dec, uint64_t out[AUDF_AAC_PROF_COUNT]) {
    aac_dec_t *d = (aac_dec_t *)dec;
    if (!d || !d->clock) return false;
    memcpy(out, d->prof, sizeof(d->prof));
    memset(d->prof, 0, sizeof(d->prof));
    return true;
}

uint32_t audf_aac_decoder_rate(const audf_decoder_t *dec) {
    return dec ? ((const aac_dec_t *)dec)->out_rate : 0;
}
