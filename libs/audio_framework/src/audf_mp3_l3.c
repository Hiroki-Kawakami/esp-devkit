/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <string.h>

#include "audf_mp3_internal.h"

#define XR_MAX (1 << 27)

typedef struct {
    uint16_t part23;
    uint16_t big_values;
    uint16_t sfc;
    uint8_t  global_gain;
    uint8_t  block_type;
    uint8_t  mixed;
    uint8_t  table[3];
    uint8_t  sbg[3];
    uint8_t  region0;
    uint8_t  region1;
    uint8_t  preflag;
    uint8_t  sfscale;
    uint8_t  c1table;
} granule_t;

typedef struct {
    uint16_t  main_begin;
    uint8_t   scfsi[2];
    granule_t gr[2][2];
} side_t;

typedef struct {
    uint8_t n;
    uint8_t n_long;
    uint8_t width[40];
    uint8_t sf[40];
} bands_t;

static esp_err_t read_side(const mp3_frame_t *f, mp3_bits_t *b, side_t *s) {
    const bool lsf = f->version != 0;
    const int nch = f->h.channels;
    s->main_begin = (uint16_t)mp3_bits_get(b, lsf ? 8 : 9);
    b->pos += lsf ? (nch == 1 ? 1 : 2) : (nch == 1 ? 5 : 3);
    for (int ch = 0; ch < nch; ch++) s->scfsi[ch] = lsf ? 0 : (uint8_t)mp3_bits_get(b, 4);
    for (int gr = 0; gr < (lsf ? 1 : 2); gr++) {
        for (int ch = 0; ch < nch; ch++) {
            granule_t *g = &s->gr[gr][ch];
            g->part23 = (uint16_t)mp3_bits_get(b, 12);
            g->big_values = (uint16_t)mp3_bits_get(b, 9);
            if (g->big_values > 288) return ESP_ERR_INVALID_RESPONSE;
            g->global_gain = (uint8_t)mp3_bits_get(b, 8);
            g->sfc = (uint16_t)mp3_bits_get(b, lsf ? 9 : 4);
            if (mp3_bits_get(b, 1)) {
                g->block_type = (uint8_t)mp3_bits_get(b, 2);
                if (!g->block_type) return ESP_ERR_INVALID_RESPONSE;
                g->mixed = (uint8_t)mp3_bits_get(b, 1);
                g->table[0] = (uint8_t)mp3_bits_get(b, 5);
                g->table[1] = (uint8_t)mp3_bits_get(b, 5);
                g->table[2] = 0;
                for (int w = 0; w < 3; w++) g->sbg[w] = (uint8_t)mp3_bits_get(b, 3);
                g->region0 = g->region1 = 0;
            } else {
                g->block_type = 0;
                g->mixed = 0;
                for (int r = 0; r < 3; r++) g->table[r] = (uint8_t)mp3_bits_get(b, 5);
                g->region0 = (uint8_t)mp3_bits_get(b, 4);
                g->region1 = (uint8_t)mp3_bits_get(b, 3);
                g->sbg[0] = g->sbg[1] = g->sbg[2] = 0;
            }
            g->preflag = lsf ? 0 : (uint8_t)mp3_bits_get(b, 1);
            g->sfscale = (uint8_t)mp3_bits_get(b, 1);
            g->c1table = (uint8_t)mp3_bits_get(b, 1);
        }
    }
    return ESP_OK;
}

static void build_bands(const mp3_frame_t *f, const granule_t *g, bands_t *bb) {
    const uint8_t *L = mp3_sfb_long[f->rate_index];
    const uint8_t *S = mp3_sfb_short[f->rate_index];
    int n = 0;
    if (g->block_type != 2) {
        for (int i = 0; i < 22; i++) bb->width[n++] = L[i];
        bb->n = bb->n_long = (uint8_t)n;
        return;
    }
    int first = 0;
    if (g->mixed) {
        for (int i = 0, lines = 0; lines < 36; lines += L[i++]) bb->width[n++] = L[i];
        first = 12;
    }
    bb->n_long = (uint8_t)n;
    for (int i = 0, start = 0; i < 13; i++) {
        int lo = start;
        const int hi = start + S[i];
        start = hi;
        if (hi <= first) continue;
        if (lo < first) lo = first;
        for (int w = 0; w < 3; w++) bb->width[n++] = (uint8_t)(hi - lo);
    }
    bb->n = (uint8_t)n;
}

static void read_sf_mpeg1(mp3_dec_t *d, mp3_bits_t *b, const granule_t *g, int gr, int ch, uint8_t scfsi,
                          bands_t *bb) {
    const unsigned slen1 = mp3_slen[g->sfc][0], slen2 = mp3_slen[g->sfc][1];
    uint8_t *sf = bb->sf;
    if (g->block_type == 2) {
        const int first = g->mixed ? 17 : 18;
        int i = 0;
        for (; i < first; i++) sf[i] = (uint8_t)mp3_bits_get(b, slen1);
        for (; i < bb->n - 3; i++) sf[i] = (uint8_t)mp3_bits_get(b, slen2);
        for (; i < bb->n; i++) sf[i] = 0;
        return;
    }
    static const uint8_t s_group[5] = { 0, 6, 11, 16, 21 };
    for (int grp = 0; grp < 4; grp++) {
        const bool reuse = gr == 1 && (scfsi >> (3 - grp)) & 1;
        const unsigned slen = grp < 2 ? slen1 : slen2;
        for (int i = s_group[grp]; i < s_group[grp + 1]; i++) {
            sf[i] = reuse ? d->scf_prev[ch][i] : (uint8_t)mp3_bits_get(b, slen);
        }
    }
    sf[21] = 0;
    memcpy(d->scf_prev[ch], sf, 22);
}

static void read_sf_lsf(mp3_bits_t *b, granule_t *g, bool is_right, bands_t *bb) {
    unsigned slen[4] = { 0 };
    int table;
    unsigned sfc = g->sfc;
    if (!is_right) {
        if (sfc < 400) {
            slen[0] = (sfc >> 4) / 5;
            slen[1] = (sfc >> 4) % 5;
            slen[2] = (sfc & 15) >> 2;
            slen[3] = sfc & 3;
            table = 0;
        } else if (sfc < 500) {
            sfc -= 400;
            slen[0] = (sfc >> 2) / 5;
            slen[1] = (sfc >> 2) % 5;
            slen[2] = sfc & 3;
            table = 1;
        } else {
            sfc -= 500;
            slen[0] = sfc / 3;
            slen[1] = sfc % 3;
            g->preflag = 1;
            table = 2;
        }
    } else {
        sfc >>= 1;
        if (sfc < 180) {
            slen[0] = sfc / 36;
            slen[1] = (sfc % 36) / 6;
            slen[2] = (sfc % 36) % 6;
            table = 3;
        } else if (sfc < 244) {
            sfc -= 180;
            slen[0] = (sfc & 63) >> 4;
            slen[1] = (sfc & 15) >> 2;
            slen[2] = sfc & 3;
            table = 4;
        } else {
            sfc -= 244;
            slen[0] = sfc / 3;
            slen[1] = sfc % 3;
            table = 5;
        }
    }
    const int blk = g->block_type == 2 ? (g->mixed ? 2 : 1) : 0;
    const uint8_t *nr = mp3_lsf_nsfb[table][blk];
    int i = 0;
    for (int p = 0; p < 4; p++) {
        for (int j = 0; j < nr[p] && i < bb->n; j++) bb->sf[i++] = (uint8_t)mp3_bits_get(b, slen[p]);
    }
    for (; i < bb->n; i++) bb->sf[i] = 0;
}

static inline unsigned huff(mp3_bits_t *b, const uint16_t *lut, unsigned bits) {
    const uint32_t w = mp3_bits_show(b);
    unsigned base = 0;
    uint32_t e = lut[w >> (32 - bits)];
    while (e >> 15) {
        base += bits;
        bits = (e >> 12) & 7;
        e = lut[(e & 0xfff) + ((w << base) >> (32 - bits))];
    }
    b->pos += base + (e >> 9);
    return e & 0x1ff;
}

static inline int32_t signed_value(mp3_bits_t *b, unsigned v, unsigned linbits) {
    if (v == 15 && linbits) v += mp3_bits_get(b, linbits);
    if (!v) return 0;
    return mp3_bits_get(b, 1) ? -(int32_t)v : (int32_t)v;
}

static int read_huffman(const mp3_frame_t *f, mp3_bits_t *b, const granule_t *g, uint32_t part_end,
                        int32_t *is) {
    const uint8_t *L = mp3_sfb_long[f->rate_index];
    int bound[3];
    if (g->block_type) {
        const uint8_t *S = mp3_sfb_short[f->rate_index];
        bound[0] = g->block_type == 2 ? 3 * (S[0] + S[1] + S[2]) : 0;
        if (g->block_type != 2) {
            for (int i = 0; i < 8; i++) bound[0] += L[i];
        }
        bound[1] = 576;
    } else {
        int line = 0, i = 0;
        for (; i <= g->region0 && i < 22; i++) line += L[i];
        bound[0] = i <= g->region0 ? 576 : line;
        for (; i <= g->region0 + g->region1 + 1 && i < 22; i++) line += L[i];
        bound[1] = i <= g->region0 + g->region1 + 1 ? 576 : line;
    }
    bound[2] = 576;

    const int big = g->big_values * 2;
    int i = 0;
    for (int r = 0; r < 3; r++) {
        const int end = bound[r] < big ? bound[r] : big;
        const mp3_huff_t *h = &mp3_huff_pairs[g->table[r]];
        if (!h->root_bits) {
            for (; i < end; i++) is[i] = 0;
            continue;
        }
        const uint16_t *lut = mp3_huff_lut + h->offset;
        for (; i < end; i += 2) {
            const unsigned v = huff(b, lut, h->root_bits);
            is[i] = signed_value(b, v >> 4, h->linbits);
            is[i + 1] = signed_value(b, v & 15, h->linbits);
        }
    }

    const uint16_t *quad = mp3_huff_lut + mp3_huff_quad.offset;
    while (i + 4 <= 576 && b->pos < part_end) {
        const unsigned v = g->c1table ? 15 - mp3_bits_get(b, 4) : huff(b, quad, mp3_huff_quad.root_bits);
        int32_t q[4];
        for (int k = 0; k < 4; k++) q[k] = signed_value(b, (v >> (3 - k)) & 1, 0);
        if (b->pos > part_end) break;
        memcpy(is + i, q, sizeof(q));
        i += 4;
    }
    const int nz = i;
    for (; i < 576; i++) is[i] = 0;
    return nz;
}

static int64_t pow43_q17(int32_t q) {
    if (q < 1024) return mp3_pow43_q17[q];
    const int a = q >> 3, r = q & 7;
    const int64_t lo = mp3_pow43_q17[a], hi = mp3_pow43_q17[a + 1];
    return 2 * (8 * lo + r * (hi - lo));
}

static void requantize(const granule_t *g, const bands_t *bb, int32_t *x, int nz) {
    const int sh = g->sfscale ? 4 : 2;
    int line = 0;
    for (int i = 0; i < bb->n && line < nz; i++) {
        int e = g->global_gain - 210;
        if (i < bb->n_long) {
            e -= sh * (bb->sf[i] + (g->preflag ? mp3_pretab[i] : 0));
        } else {
            e -= 8 * g->sbg[(i - bb->n_long) % 3] + sh * bb->sf[i];
        }
        const int64_t frac = mp3_pow2_quarter_q27[e & 3];
        const int shift = (e >> 2) + MP3_FRAC - 17 - 27;
        const int end = line + bb->width[i];
        for (; line < end; line++) {
            const int32_t q = x[line];
            if (!q) continue;
            const int64_t p = pow43_q17(q < 0 ? -q : q) * frac;
            int64_t v;
            if (shift >= 0) {
                v = shift > 30 || p > ((int64_t)XR_MAX >> shift) ? XR_MAX : p << shift;
            } else {
                v = -shift > 62 ? 0 : (p + ((int64_t)1 << (-shift - 1))) >> -shift;
            }
            if (v > XR_MAX) v = XR_MAX;
            x[line] = q < 0 ? -(int32_t)v : (int32_t)v;
        }
    }
}

static inline int32_t clamp_xr(int64_t v) {
    return v > XR_MAX ? XR_MAX : v < -XR_MAX ? -XR_MAX : (int32_t)v;
}

static void mid_side(int32_t *l, int32_t *r, int n) {
    static const int32_t inv_sqrt2 = 1518500250;
    for (int i = 0; i < n; i++) {
        const int64_t m = l[i], s = r[i];
        l[i] = clamp_xr(((m + s) * inv_sqrt2) >> 31);
        r[i] = clamp_xr(((m - s) * inv_sqrt2) >> 31);
    }
}

static void stereo(const mp3_frame_t *f, const granule_t *gr, bands_t *bands, int32_t (*xr)[576], int *nz) {
    const bool ms = f->mode_ext & 2, is = f->mode_ext & 1;
    const int n = nz[0] > nz[1] ? nz[0] : nz[1];
    nz[0] = nz[1] = n;
    if (!is) {
        if (ms) mid_side(xr[0], xr[1], n);
        return;
    }

    bands_t *bb = &bands[1];
    const bool lsf = f->version != 0;
    int last[4] = { -1, -1, -1, -1 };
    for (int i = 0, line = 0; i < bb->n; line += bb->width[i++]) {
        for (int k = 0; k < bb->width[i]; k++) {
            if (xr[1][line + k]) {
                last[i < bb->n_long ? 3 : (i - bb->n_long) % 3] = i;
                break;
            }
        }
    }
    const bool shorts = bb->n_long < bb->n;
    const bool long_is = !shorts || (last[0] < 0 && last[1] < 0 && last[2] < 0);
    bool use[40];
    for (int i = 0; i < bb->n; i++) {
        const bool above = i < bb->n_long ? long_is && i > last[3] : i > last[(i - bb->n_long) % 3];
        use[i] = above && (lsf || bb->sf[i] < 7);
    }
    const int blocks = shorts ? 3 : 1;
    for (int w = 0; w < blocks; w++) {
        const int top = bb->n - blocks + w, prev = top - blocks;
        if (!use[top]) continue;
        use[top] = use[prev];
        bb->sf[top] = bb->sf[prev];
    }

    const int scale = gr[1].sfc & 1;
    for (int i = 0, line = 0; i < bb->n; line += bb->width[i++]) {
        const unsigned pos = bb->sf[i];
        int32_t *l = xr[0] + line, *r = xr[1] + line;
        const int w = bb->width[i];
        if (use[i]) {
            int32_t kl, kr;
            if (!lsf) {
                kl = mp3_is_ratio_q30[2 * pos];
                kr = mp3_is_ratio_q30[2 * pos + 1];
            } else {
                const unsigned e = ((pos + 1) >> 1) << scale;
                const int32_t k = e >> 2 > 30 ? 0 : mp3_pow2_neg_quarter_q30[e & 3] >> (e >> 2);
                kl = pos & 1 ? k : 1 << 30;
                kr = pos & 1 ? 1 << 30 : k;
            }
            for (int k = 0; k < w; k++) {
                const int64_t v = l[k];
                l[k] = (int32_t)((v * kl) >> 30);
                r[k] = (int32_t)((v * kr) >> 30);
            }
        } else if (ms) {
            mid_side(l, r, w);
        }
    }
}

static void reorder(const bands_t *bb, int32_t *x, int32_t *tmp, bool mixed) {
    const int start = mixed ? 36 : 0;
    int line = start, f0 = mixed ? 12 : 0;
    for (int i = bb->n_long; i < bb->n; i += 3) {
        const int w = bb->width[i];
        for (int win = 0; win < 3; win++) {
            for (int j = 0; j < w; j++) {
                const int fl = f0 + j;
                tmp[18 * (fl / 6) + 6 * win + fl % 6] = x[line++];
            }
        }
        f0 += w;
    }
    memcpy(x + start, tmp + start, (576 - start) * sizeof(int32_t));
}

static void antialias(int32_t *x, int sblimit) {
    for (int sb = 1; sb < sblimit; sb++) {
        int32_t *lo = x + 18 * sb - 1, *hi = x + 18 * sb;
        for (int i = 0; i < 8; i++) {
            const int32_t a = lo[-i], b = hi[i];
            lo[-i] = clamp_xr(2 * (int64_t)(mp3_mulh(a, mp3_alias_cs[i]) - mp3_mulh(b, mp3_alias_ca[i])));
            hi[i] = clamp_xr(2 * (int64_t)(mp3_mulh(b, mp3_alias_cs[i]) + mp3_mulh(a, mp3_alias_ca[i])));
        }
    }
}

static void imdct36(const int32_t *in, int32_t *ov, int32_t *out, const int32_t *win) {
    int32_t x[18], z[18];
    const int s = mp3_headroom(in, 18, 27);
    const int32_t round = 1 << s >> 1;
    for (int k = 0; k < 18; k++) x[k] = in[k] * (1 << s);
    for (int m = 0; m < 18; m++) {
        const int32_t *c = mp3_imdct18 + 18 * m;
        int32_t acc = 0;
#pragma GCC unroll 18
        for (int k = 0; k < 18; k++) acc += mp3_mulh(x[k], c[k]);
        z[m] = (2 * acc + round) >> s;
    }
    for (int n = 0; n < 18; n++) {
        const int32_t y = n < 9 ? z[n + 9] : -z[26 - n];
        out[32 * n] = mp3_sat((int64_t)mp3_mul31(y, win[n]) + ov[n]);
    }
    for (int n = 18; n < 36; n++) {
        const int32_t y = n < 27 ? -z[26 - n] : -z[n - 27];
        ov[n - 18] = mp3_mul31(y, win[n]);
    }
}

static void imdct12x3(const int32_t *in, int32_t *ov, int32_t *out) {
    const int32_t *win = mp3_imdct_window[2];
    int32_t buf[36] = { 0 };
    int32_t x[18];
    const int s = mp3_headroom(in, 18, 27);
    const int32_t round = 1 << s >> 1;
    for (int k = 0; k < 18; k++) x[k] = in[k] * (1 << s);
    for (int w = 0; w < 3; w++) {
        int32_t z[6];
        for (int m = 0; m < 6; m++) {
            const int32_t *c = mp3_imdct6 + 6 * m;
            int32_t acc = 0;
#pragma GCC unroll 6
            for (int k = 0; k < 6; k++) acc += mp3_mulh(x[6 * w + k], c[k]);
            z[m] = (2 * acc + round) >> s;
        }
        for (int n = 0; n < 12; n++) {
            const int32_t y = n < 3 ? z[n + 3] : n < 9 ? -z[8 - n] : -z[n - 9];
            buf[6 + 6 * w + n] = mp3_sat((int64_t)buf[6 + 6 * w + n] + mp3_mul31(y, win[n]));
        }
    }
    for (int n = 0; n < 18; n++) {
        out[32 * n] = mp3_sat((int64_t)buf[n] + ov[n]);
        ov[n] = buf[18 + n];
    }
}

static void hybrid(mp3_dec_t *d, int ch, const granule_t *g, int nsb) {
    const int32_t *x = d->xr[ch];
    int32_t *ov = d->overlap[ch];
    int32_t (*out)[32] = d->sb[ch];
    for (int sb = 0; sb < 32; sb++) {
        int32_t *o = &out[0][sb];
        if (sb >= nsb) {
            for (int t = 0; t < 18; t++) {
                o[32 * t] = ov[18 * sb + t];
                ov[18 * sb + t] = 0;
            }
        } else {
            const int bt = g->mixed && sb < 2 ? 0 : g->block_type;
            if (bt == 2) {
                imdct12x3(x + 18 * sb, ov + 18 * sb, o);
            } else {
                imdct36(x + 18 * sb, ov + 18 * sb, o, mp3_imdct_window[bt]);
            }
        }
        if (sb & 1) {
            for (int t = 1; t < 18; t += 2) o[32 * t] = -o[32 * t];
        }
    }
}

esp_err_t mp3_l3_decode(mp3_dec_t *d, int16_t *pcm) {
    const mp3_frame_t *f = &d->f;
    const int nch = f->h.channels;
    const bool lsf = f->version != 0;
    mp3_bits_t b = { d->frame, f->crc ? 48 : 32, f->h.bytes * 8u };
    side_t side;
    const esp_err_t err = read_side(f, &b, &side);
    if (err != ESP_OK) return err;

    const uint32_t start = b.pos >> 3;
    if (start > f->h.bytes) return ESP_ERR_INVALID_SIZE;
    const uint32_t len = f->h.bytes - start;
    memcpy(d->main + d->reservoir, d->frame + start, len);
    const uint32_t total = d->reservoir + len;
    memset(d->main + total, 0, MP3_PAD);
    const bool have = side.main_begin <= d->reservoir;
    const uint32_t base = have ? d->reservoir - side.main_begin : 0;
    mp3_bits_t m = { d->main + base, 0, (total - base) * 8 };

    const bool joint = f->mode == MP3_JOINT && nch == 2;
    const int out_frame = 576 * d->base.channels;
    uint32_t pos = 0;
    for (int gr = 0; gr < (lsf ? 1 : 2); gr++) {
        bands_t bands[2];
        int nz[2] = { 0, 0 };
        granule_t *g = side.gr[gr];
        for (int ch = 0; ch < nch; ch++) {
            build_bands(f, &g[ch], &bands[ch]);
            if (!have) {
                memset(d->xr[ch], 0, sizeof(d->xr[ch]));
                memset(bands[ch].sf, 0, sizeof(bands[ch].sf));
                continue;
            }
            m.pos = pos;
            const uint32_t end = pos + g[ch].part23;
            if (lsf) {
                read_sf_lsf(&m, &g[ch], joint && (f->mode_ext & 1) && ch == 1, &bands[ch]);
            } else {
                read_sf_mpeg1(d, &m, &g[ch], gr, ch, side.scfsi[ch], &bands[ch]);
            }
            nz[ch] = read_huffman(f, &m, &g[ch], end, d->xr[ch]);
            requantize(&g[ch], &bands[ch], d->xr[ch], nz[ch]);
            pos = end;
        }
        if (joint && have) stereo(f, g, bands, d->xr, nz);
        for (int ch = 0; ch < nch; ch++) {
            int nsb = 32;
            if (g[ch].block_type == 2) {
                reorder(&bands[ch], d->xr[ch], d->scratch, g[ch].mixed);
                if (g[ch].mixed) antialias(d->xr[ch], 2);
            } else {
                const int used = (nz[ch] + 17) / 18;
                nsb = used < 31 ? used + 1 : 32;
                antialias(d->xr[ch], nsb);
            }
            hybrid(d, ch, &g[ch], nsb);
        }
        mp3_emit(d, 18, pcm + gr * out_frame);
    }

    const uint32_t keep = total < MP3_MAX_BACK ? total : MP3_MAX_BACK;
    memmove(d->main, d->main + total - keep, keep);
    d->reservoir = keep;
    return ESP_OK;
}
