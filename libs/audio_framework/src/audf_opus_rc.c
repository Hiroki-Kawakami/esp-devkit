/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_opus_internal.h"

#define RC_TOP (1u << 31)
#define RC_BOT (1u << 23)

static int rc_byte(opus_rc_t *rc) {
    return rc->offs < rc->storage ? rc->buf[rc->offs++] : 0;
}

static int rc_byte_from_end(opus_rc_t *rc) {
    return rc->end_offs < rc->storage ? rc->buf[rc->storage - ++rc->end_offs] : 0;
}

static void rc_normalize(opus_rc_t *rc) {
    while (rc->rng <= RC_BOT) {
        rc->nbits_total += 8;
        rc->rng <<= 8;
        const int prev = rc->rem;
        rc->rem = rc_byte(rc);
        const int sym = (prev << 8 | rc->rem) >> 1;
        rc->val = ((rc->val << 8) + (255 & ~sym)) & (RC_TOP - 1);
    }
}

void opus_rc_init(opus_rc_t *rc, const uint8_t *buf, uint32_t len) {
    rc->buf = buf;
    rc->storage = len;
    rc->end_offs = 0;
    rc->end_window = 0;
    rc->nend_bits = 0;
    rc->nbits_total = 9;
    rc->offs = 0;
    rc->rng = 128;
    rc->rem = rc_byte(rc);
    rc->val = 127 - (rc->rem >> 1);
    rc->error = 0;
    rc_normalize(rc);
}

uint32_t opus_rc_decode(opus_rc_t *rc, uint32_t ft) {
    rc->ext = rc->rng / ft;
    const uint32_t s = rc->val / rc->ext;
    return ft - (s + 1 < ft ? s + 1 : ft);
}

uint32_t opus_rc_decode_bin(opus_rc_t *rc, int bits) {
    rc->ext = rc->rng >> bits;
    const uint32_t s = rc->val / rc->ext;
    const uint32_t ft = 1u << bits;
    return ft - (s + 1 < ft ? s + 1 : ft);
}

void opus_rc_update(opus_rc_t *rc, uint32_t fl, uint32_t fh, uint32_t ft) {
    const uint32_t s = rc->ext * (ft - fh);
    rc->val -= s;
    rc->rng = fl > 0 ? rc->ext * (fh - fl) : rc->rng - s;
    rc_normalize(rc);
}

int opus_rc_bit_logp(opus_rc_t *rc, int logp) {
    const uint32_t s = rc->rng >> logp;
    const int bit = rc->val < s;
    if (bit) {
        rc->rng = s;
    } else {
        rc->val -= s;
        rc->rng -= s;
    }
    rc_normalize(rc);
    return bit;
}

int opus_rc_icdf(opus_rc_t *rc, const uint8_t *icdf, int ftb) {
    const uint32_t r = rc->rng >> ftb;
    uint32_t s = rc->rng;
    uint32_t t;
    int k = -1;
    do {
        t = s;
        s = r * icdf[++k];
    } while (rc->val < s);
    rc->val -= s;
    rc->rng = t - s;
    rc_normalize(rc);
    return k;
}

uint32_t opus_rc_bits(opus_rc_t *rc, int bits) {
    uint32_t window = rc->end_window;
    int available = rc->nend_bits;
    if (available < bits) {
        do {
            window |= (uint32_t)rc_byte_from_end(rc) << available;
            available += 8;
        } while (available <= 24);
    }
    const uint32_t v = window & ((1u << bits) - 1);
    rc->end_window = window >> bits;
    rc->nend_bits = available - bits;
    rc->nbits_total += bits;
    return v;
}

uint32_t opus_rc_uint(opus_rc_t *rc, uint32_t ft) {
    const uint32_t top = ft - 1;
    const int ftb = opus_ilog(top);
    if (ftb > 8) {
        const int raw = ftb - 8;
        const uint32_t fth = (top >> raw) + 1;
        const uint32_t s = opus_rc_decode(rc, fth);
        opus_rc_update(rc, s, s + 1, fth);
        const uint32_t t = s << raw | opus_rc_bits(rc, raw);
        if (t <= top) return t;
        rc->error = 1;
        return top;
    }
    const uint32_t s = opus_rc_decode(rc, ft);
    opus_rc_update(rc, s, s + 1, ft);
    return s;
}

uint32_t opus_rc_tell_frac(const opus_rc_t *rc) {
    int lg = opus_ilog(rc->rng);
    uint32_t r = rc->rng >> (lg - 16);
    for (int i = 0; i < 3; i++) {
        r = r * r >> 15;
        const int b = (int)(r >> 16);
        lg = lg << 1 | b;
        r >>= b;
    }
    return ((uint32_t)rc->nbits_total << 3) - (uint32_t)lg;
}
