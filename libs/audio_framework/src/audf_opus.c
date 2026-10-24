/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_opus.h"

#include <string.h>

#include "audf_alloc.h"
#include "audf_codec_internal.h"
#include "audf_opus_internal.h"

#define MAX_FRAMES 5760
#define F20 960
#define F10 480
#define F5 240
#define F2_5 120

enum {
    MODE_NONE = 0,
    MODE_SILK,
    MODE_HYBRID,
    MODE_CELT,
};

enum {
    BW_NB = 0,
    BW_MB,
    BW_WB,
    BW_SWB,
    BW_FB,
};

typedef struct {
    audf_decoder_t base;
    uint16_t pre_skip;
    int32_t  gain_q16;
    int      mode;
    int      prev_mode;
    int      bandwidth;
    int      frame_size;
    int      stream_channels;
    bool     prev_redundancy;
    uint32_t final_range;
    celt_dec_t  celt;
    silk_dec_t *silk;
    int32_t *pcm;
    int32_t *transition;
    int32_t *redundant;
    int16_t *silk_pcm;
} opus_decoder_t;

static int toc_mode(uint8_t toc) {
    if (toc & 0x80) return MODE_CELT;
    return (toc & 0x60) == 0x60 ? MODE_HYBRID : MODE_SILK;
}

static int toc_bandwidth(uint8_t toc) {
    if (toc & 0x80) {
        const int bw = BW_MB + ((toc >> 5) & 3);
        return bw == BW_MB ? BW_NB : bw;
    }
    if ((toc & 0x60) == 0x60) return toc & 0x10 ? BW_FB : BW_SWB;
    return (toc >> 5) & 3;
}

static int toc_frame_size(uint8_t toc) {
    if (toc & 0x80) return 120 << ((toc >> 3) & 3);
    if ((toc & 0x60) == 0x60) return toc & 0x08 ? 960 : 480;
    const int size = (toc >> 3) & 3;
    return size == 3 ? 2880 : 480 << size;
}

static int read_size(const uint8_t *p, int len, int *size) {
    if (len < 1) return -1;
    if (p[0] < 252) {
        *size = p[0];
        return 1;
    }
    if (len < 2) return -1;
    *size = 4 * p[1] + p[0];
    return 2;
}

/* RFC 6716 3.2: frame offsets and sizes of a packet, or -1 if malformed. */
static int parse_packet(const uint8_t *data, int len, const uint8_t *frames[48], int sizes[48]) {
    if (len < 1) return -1;
    const int frame_size = toc_frame_size(data[0]);
    const uint8_t toc = *data++;
    len--;
    int count;
    int last;
    switch (toc & 3) {
    case 0:
        count = 1;
        last = len;
        break;
    case 1:
        if (len & 1) return -1;
        count = 2;
        sizes[0] = last = len / 2;
        break;
    case 2: {
        count = 2;
        const int bytes = read_size(data, len, &sizes[0]);
        if (bytes < 0) return -1;
        len -= bytes;
        if (sizes[0] > len) return -1;
        data += bytes;
        last = len - sizes[0];
        break;
    }
    default: {
        if (len < 1) return -1;
        const uint8_t ch = *data++;
        len--;
        count = ch & 0x3F;
        if (count <= 0 || frame_size * count > MAX_FRAMES) return -1;
        if (ch & 0x40) {
            int p;
            do {
                if (len <= 0) return -1;
                p = *data++;
                len--;
                len -= p == 255 ? 254 : p;
            } while (p == 255);
        }
        if (len < 0) return -1;
        if (ch & 0x80) {
            last = len;
            for (int i = 0; i < count - 1; i++) {
                const int bytes = read_size(data, len, &sizes[i]);
                if (bytes < 0) return -1;
                len -= bytes;
                if (sizes[i] > len) return -1;
                data += bytes;
                last -= bytes + sizes[i];
            }
            if (last < 0) return -1;
        } else {
            last = len / count;
            if (last * count != len) return -1;
            for (int i = 0; i < count - 1; i++) sizes[i] = last;
        }
        break;
    }
    }
    if (last > 1275) return -1;
    sizes[count - 1] = last;
    for (int i = 0; i < count; i++) {
        frames[i] = data;
        data += sizes[i];
    }
    return count;
}

static void smooth_fade(const int32_t *in1, const int32_t *in2, int32_t *out, int n, int channels) {
    for (int c = 0; c < channels; c++) {
        for (int i = 0; i < n; i++) {
            const int32_t w = celt_window[i] * celt_window[i] >> 15;
            const int64_t v = (int64_t)w * in2[i * channels + c] + (int64_t)(32767 - w) * in1[i * channels + c];
            out[i * channels + c] = (int32_t)(v >> 15);
        }
    }
}

static int celt_lm(int frame_size) {
    int lm = 0;
    while ((120 << lm) < frame_size) lm++;
    return lm;
}

static int decode_frame(opus_decoder_t *d, const uint8_t *data, int len, int32_t *pcm, int frame_size) {
    const int channels = d->base.channels;
    celt_dec_t *celt = &d->celt;
    opus_rc_t rc;
    int audiosize;
    int mode;
    bool transition = false;
    bool redundancy = false;
    bool celt_to_silk = false;
    int redundancy_bytes = 0;
    uint32_t redundant_rng = 0;

    if (len <= 1) {
        data = NULL;
        frame_size = opus_imin(frame_size, d->frame_size);
    }
    if (data) {
        audiosize = d->frame_size;
        mode = d->mode;
        opus_rc_init(&rc, data, (uint32_t)len);
    } else {
        audiosize = frame_size;
        if (d->prev_mode == MODE_NONE) {
            memset(pcm, 0, (size_t)audiosize * channels * sizeof(*pcm));
            return audiosize;
        }
        mode = d->prev_mode;
    }

    if (!data && frame_size > F20 && mode != MODE_SILK) {
        for (int done = 0; done < frame_size; done += F20) {
            decode_frame(d, NULL, 0, pcm + done * channels, F20);
        }
        return frame_size;
    }

    int32_t *trans = d->transition;
    if (data && d->prev_mode != MODE_NONE &&
        ((mode == MODE_CELT && d->prev_mode != MODE_CELT && !d->prev_redundancy) ||
         (mode != MODE_CELT && d->prev_mode == MODE_CELT))) {
        transition = true;
        if (mode == MODE_CELT) decode_frame(d, NULL, 0, trans, opus_imin(F5, audiosize));
    }
    frame_size = audiosize;

    int16_t *silk_pcm = d->silk_pcm;
    if (mode != MODE_CELT) {
        if (d->prev_mode == MODE_CELT) silk_reset(d->silk);
        const int payload_ms = opus_imax(10, audiosize / 48);
        int internal_khz = 16;
        if (mode == MODE_SILK) {
            if (d->bandwidth == BW_NB) internal_khz = 8;
            if (d->bandwidth == BW_MB) internal_khz = 12;
        }
        int done = 0;
        do {
            const int n = silk_decode(d->silk, data ? &rc : NULL, !data, done == 0, internal_khz,
                                      d->stream_channels, payload_ms, silk_pcm + done * channels);
            if (n <= 0) {
                if (data) return -1;
                memset(silk_pcm + done * channels, 0, (size_t)(frame_size - done) * channels * sizeof(int16_t));
                break;
            }
            done += n;
        } while (done < frame_size);
    }

    int start_band = 0;
    if (data && mode != MODE_CELT && opus_rc_tell(&rc) + 17 + 20 * (mode == MODE_HYBRID) <= 8 * len) {
        redundancy = mode == MODE_HYBRID ? opus_rc_bit_logp(&rc, 12) : true;
        if (redundancy) {
            celt_to_silk = opus_rc_bit_logp(&rc, 1);
            redundancy_bytes = mode == MODE_HYBRID ? (int)opus_rc_uint(&rc, 256) + 2
                                                   : len - ((opus_rc_tell(&rc) + 7) >> 3);
            len -= redundancy_bytes;
            if (len * 8 < opus_rc_tell(&rc)) {
                len = 0;
                redundancy_bytes = 0;
                redundancy = false;
            }
            rc.storage -= (uint32_t)redundancy_bytes;
        }
    }
    if (mode != MODE_CELT) start_band = 17;

    static const uint8_t end_bands[5] = { 13, 17, 17, 19, 21 };
    celt->end = end_bands[d->bandwidth];
    celt->stream_channels = (uint8_t)d->stream_channels;

    if (redundancy) transition = false;
    if (transition && mode != MODE_CELT) decode_frame(d, NULL, 0, trans, opus_imin(F5, audiosize));

    int32_t *red = d->redundant;
    if (redundancy && celt_to_silk) {
        opus_rc_t rrc;
        opus_rc_init(&rrc, data + len, (uint32_t)redundancy_bytes);
        celt->start = 0;
        celt_decode(celt, &rrc, (uint32_t)redundancy_bytes, 1, red);
        redundant_rng = rrc.rng;
    }

    celt->start = (uint8_t)start_band;
    if (mode != MODE_SILK) {
        const int celt_frame = opus_imin(F20, frame_size);
        if (mode != d->prev_mode && d->prev_mode != MODE_NONE && !d->prev_redundancy) celt_reset(celt);
        celt->stream_channels = (uint8_t)d->stream_channels;
        celt->end = end_bands[d->bandwidth];
        celt->start = (uint8_t)start_band;
        celt_decode(celt, data ? &rc : NULL, (uint32_t)len, celt_lm(celt_frame), pcm);
    } else {
        memset(pcm, 0, (size_t)frame_size * channels * sizeof(*pcm));
        if (d->prev_mode == MODE_HYBRID && !(redundancy && celt_to_silk && d->prev_redundancy)) {
            static const uint8_t silence[2] = { 0xFF, 0xFF };
            opus_rc_t src;
            opus_rc_init(&src, silence, 2);
            celt->start = 0;
            celt_decode(celt, &src, 2, 0, pcm);
        }
    }

    if (mode != MODE_CELT) {
        for (int i = 0; i < frame_size * channels; i++) {
            pcm[i] = celt_sat_sig((int64_t)pcm[i] + ((int32_t)silk_pcm[i] << SIG_SHIFT));
        }
    }

    if (redundancy && !celt_to_silk) {
        opus_rc_t rrc;
        opus_rc_init(&rrc, data + len, (uint32_t)redundancy_bytes);
        celt_reset(celt);
        celt->start = 0;
        celt->stream_channels = (uint8_t)d->stream_channels;
        celt->end = end_bands[d->bandwidth];
        celt_decode(celt, &rrc, (uint32_t)redundancy_bytes, 1, red);
        redundant_rng = rrc.rng;
        smooth_fade(pcm + channels * (frame_size - F2_5), red + channels * F2_5,
                    pcm + channels * (frame_size - F2_5), F2_5, channels);
    }
    if (redundancy && celt_to_silk) {
        memcpy(pcm, red, (size_t)F2_5 * channels * sizeof(*pcm));
        smooth_fade(red + channels * F2_5, pcm + channels * F2_5, pcm + channels * F2_5, F2_5, channels);
    }
    if (transition) {
        if (audiosize >= F5) {
            memcpy(pcm, trans, (size_t)F2_5 * channels * sizeof(*pcm));
            smooth_fade(trans + channels * F2_5, pcm + channels * F2_5, pcm + channels * F2_5, F2_5, channels);
        } else {
            smooth_fade(trans, pcm, pcm, F2_5, channels);
        }
    }

    d->final_range = len <= 1 ? 0 : rc.rng ^ redundant_rng;
    d->prev_mode = mode;
    d->prev_redundancy = redundancy && !celt_to_silk;
    return audiosize;
}

static esp_err_t decode(audf_decoder_t *dec, const void *frame, size_t len, void *pcm, size_t *frames) {
    opus_decoder_t *d = (opus_decoder_t *)dec;
    const uint8_t *data = frame;
    const int channels = dec->channels;
    int total = 0;

    if (len == 0) {
        total = decode_frame(d, NULL, 0, d->pcm, d->frame_size);
    } else {
        const uint8_t *frame_data[48];
        int sizes[48];
        const int count = parse_packet(data, (int)len, frame_data, sizes);
        if (count < 0) return ESP_ERR_INVALID_RESPONSE;
        d->mode = toc_mode(data[0]);
        d->bandwidth = toc_bandwidth(data[0]);
        d->frame_size = toc_frame_size(data[0]);
        d->stream_channels = data[0] & 4 ? 2 : 1;
        for (int i = 0; i < count; i++) {
            const int n = decode_frame(d, frame_data[i], sizes[i], d->pcm + total * channels, d->frame_size);
            if (n < 0) return ESP_ERR_INVALID_RESPONSE;
            total += n;
        }
    }

    int16_t *out = pcm;
    const int64_t round = (int64_t)1 << (15 + SIG_SHIFT);
    for (int i = 0; i < total * channels; i++) {
        const int64_t v = ((int64_t)d->pcm[i] * d->gain_q16 + round) >> (16 + SIG_SHIFT);
        out[i] = (int16_t)(v > 32767 ? 32767 : v < -32768 ? -32768 : v);
    }
    *frames = (size_t)total;
    return ESP_OK;
}

static void reset(audf_decoder_t *dec) {
    opus_decoder_t *d = (opus_decoder_t *)dec;
    celt_reset(&d->celt);
    silk_reset(d->silk);
    d->mode = d->prev_mode = MODE_NONE;
    d->prev_redundancy = false;
    d->frame_size = F2_5 * 8;
    d->stream_channels = dec->channels;
    d->final_range = 0;
}

static void destroy(audf_decoder_t *dec) {
    opus_decoder_t *d = (opus_decoder_t *)dec;
    celt_free(&d->celt);
    silk_destroy(d->silk);
    audf_free(d->pcm);
    audf_free(d->transition);
    audf_free(d->redundant);
    audf_free(d->silk_pcm);
    audf_free(d);
}

static const audf_decoder_ops_t s_ops = {
    .decode = decode,
    .reset = reset,
    .destroy = destroy,
};

esp_err_t audf_opus_decoder_create(const audf_opus_config_t *config, audf_decoder_t **out) {
    if (!out || !config || !config->head || config->head_len < AUDF_OPUS_HEAD_BYTES ||
        memcmp(config->head, "OpusHead", 8) != 0) {
        return ESP_ERR_INVALID_ARG;
    }
    const uint8_t *h = config->head;
    const uint8_t channels = h[9];
    if ((h[8] & 0xF0) != 0 || h[18] != 0 || channels < 1 || channels > 2) return ESP_ERR_NOT_SUPPORTED;

    opus_decoder_t *d = audf_calloc(1, sizeof(*d), config->alloc_caps);
    if (!d) return ESP_ERR_NO_MEM;
    d->base.ops = &s_ops;
    d->base.fmt = AUDF_FMT_S16;
    d->base.channels = channels;
    d->base.max_frames = MAX_FRAMES;
    d->pre_skip = (uint16_t)(h[10] | h[11] << 8);
    const int16_t gain_q8 = (int16_t)(h[16] | h[17] << 8);
    d->gain_q16 = (int32_t)(opus_exp2((float)gain_q8 * (3.32192809489f / (20.0f * 256.0f))) * 65536.0f + 0.5f);

    esp_err_t err = celt_init(&d->celt, channels, config->alloc_caps);
    if (err == ESP_OK) err = silk_create(&d->silk, channels, config->alloc_caps);
    d->pcm = audf_malloc((size_t)MAX_FRAMES * channels * sizeof(int32_t), config->alloc_caps);
    d->transition = audf_malloc((size_t)F5 * channels * sizeof(int32_t), config->alloc_caps);
    d->redundant = audf_malloc((size_t)F5 * channels * sizeof(int32_t), config->alloc_caps);
    d->silk_pcm = audf_malloc((size_t)MAX_FRAMES * channels * sizeof(int16_t), config->alloc_caps);
    if (err == ESP_OK && (!d->pcm || !d->transition || !d->redundant || !d->silk_pcm)) err = ESP_ERR_NO_MEM;
    if (err != ESP_OK) {
        destroy(&d->base);
        return err;
    }
    d->celt.stereo_inversion = channels == 2;
    reset(&d->base);
    *out = &d->base;
    return ESP_OK;
}

uint16_t audf_opus_decoder_pre_skip(const audf_decoder_t *dec) {
    return ((const opus_decoder_t *)dec)->pre_skip;
}

uint32_t audf_opus_decoder_final_range(const audf_decoder_t *dec) {
    return ((const opus_decoder_t *)dec)->final_range;
}
