/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imgf_inflate.h"

#include <string.h>

#include "imgf_alloc.h"

#define IMGF_INFLATE_WIN_SIZE 32768u
#define IMGF_INFLATE_WIN_MASK 32767u

static const uint16_t kLenBase[29] = {
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31,
    35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258};
static const uint8_t kLenExtra[29] = {
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2,
    3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0};
static const uint16_t kDistBase[30] = {
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193,
    257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577};
static const uint8_t kDistExtra[30] = {
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6,
    7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13};
static const uint8_t kClOrder[19] = {
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15};

static int next_byte(imgf_inflate_t *z) {
    if (z->in_avail == 0) {
        z->in_ptr = z->src.refill(z->src.user, &z->in_avail);
        if (z->in_avail == 0) { z->eof = 1; return -1; }
    }
    z->in_avail--;
    return *z->in_ptr++;
}

static uint32_t bits(imgf_inflate_t *z, int need) {
    uint32_t val = z->bitbuf;
    while (z->bitcnt < need) {
        int c = next_byte(z);
        if (c < 0) c = 0;
        val |= (uint32_t)c << z->bitcnt;
        z->bitcnt += 8;
    }
    z->bitbuf = val >> need;
    z->bitcnt -= need;
    return val & ((1u << need) - 1);
}

static int reverse_bits(int c, int len) {
    int r = 0;
    for (int i = 0; i < len; i++) { r = (r << 1) | (c & 1); c >>= 1; }
    return r;
}

static int decode_slow(imgf_inflate_t *z, const imgf_inflate_huff_t *h) {
    int code = 0, first = 0, index = 0;
    for (int len = 1; len <= 15; len++) {
        code |= (int)bits(z, 1);
        int count = h->count[len];
        if (code - first < count) return h->symbol[index + (code - first)];
        index += count;
        first += count;
        first <<= 1;
        code <<= 1;
    }
    return -1;
}

static int decode_sym(imgf_inflate_t *z, const imgf_inflate_huff_t *h) {
    while (z->bitcnt < IMGF_INFLATE_FAST_BITS) {
        int c = next_byte(z);
        if (c < 0) c = 0;
        z->bitbuf |= (uint32_t)c << z->bitcnt;
        z->bitcnt += 8;
    }
    uint16_t e = h->fast[z->bitbuf & (IMGF_INFLATE_FAST_SIZE - 1)];
    if (e) {
        int len = e >> 9;
        z->bitbuf >>= len;
        z->bitcnt -= len;
        return e & 0x1FF;
    }
    return decode_slow(z, h);
}

static int construct(imgf_inflate_huff_t *h, const uint8_t *lengths, int n) {
    for (int i = 0; i < 16; i++) h->count[i] = 0;
    for (int s = 0; s < n; s++) h->count[lengths[s]]++;
    for (int i = 0; i < IMGF_INFLATE_FAST_SIZE; i++) h->fast[i] = 0;
    if (h->count[0] == n) return 0;

    int left = 1;
    for (int len = 1; len <= 15; len++) {
        left <<= 1;
        left -= h->count[len];
        if (left < 0) return left;
    }

    int16_t offs[16];
    offs[1] = 0;
    for (int len = 1; len < 15; len++) offs[len + 1] = offs[len] + h->count[len];
    for (int s = 0; s < n; s++)
        if (lengths[s]) h->symbol[offs[lengths[s]]++] = (int16_t)s;

    uint16_t next_code[16];
    int code = 0;
    for (int len = 1; len <= 15; len++) {
        next_code[len] = (uint16_t)code;
        code = (code + h->count[len]) << 1;
    }
    for (int s = 0; s < n; s++) {
        int len = lengths[s];
        if (!len) continue;
        int c = next_code[len]++;
        if (len <= IMGF_INFLATE_FAST_BITS) {
            int rev = reverse_bits(c, len);
            for (int j = rev; j < IMGF_INFLATE_FAST_SIZE; j += (1 << len))
                h->fast[j] = (uint16_t)((len << 9) | s);
        }
    }
    return left;
}

static void build_fixed(imgf_inflate_t *z) {
    uint8_t lengths[288];
    int i = 0;
    for (; i < 144; i++) lengths[i] = 8;
    for (; i < 256; i++) lengths[i] = 9;
    for (; i < 280; i++) lengths[i] = 7;
    for (; i < 288; i++) lengths[i] = 8;
    construct(&z->lit, lengths, 288);

    uint8_t dlen[30];
    for (i = 0; i < 30; i++) dlen[i] = 5;
    construct(&z->dist, dlen, 30);
}

static bool build_dynamic(imgf_inflate_t *z) {
    int hlit  = (int)bits(z, 5) + 257;
    int hdist = (int)bits(z, 5) + 1;
    int hclen = (int)bits(z, 4) + 4;
    if (hlit > 286 || hdist > 30) return false;

    uint8_t cl_lengths[19] = {0};
    for (int i = 0; i < hclen; i++) cl_lengths[kClOrder[i]] = (uint8_t)bits(z, 3);
    imgf_inflate_huff_t cl;
    if (construct(&cl, cl_lengths, 19) < 0) return false;

    uint8_t lengths[286 + 30] = {0};
    int n = hlit + hdist;
    int idx = 0;
    while (idx < n) {
        int sym = decode_sym(z, &cl);
        if (sym < 0) return false;
        if (sym < 16) {
            lengths[idx++] = (uint8_t)sym;
        } else if (sym == 16) {
            if (idx == 0) return false;
            int rep = 3 + (int)bits(z, 2);
            uint8_t prev = lengths[idx - 1];
            while (rep-- && idx < n) lengths[idx++] = prev;
        } else if (sym == 17) {
            int rep = 3 + (int)bits(z, 3);
            while (rep-- && idx < n) lengths[idx++] = 0;
        } else {
            int rep = 11 + (int)bits(z, 7);
            while (rep-- && idx < n) lengths[idx++] = 0;
        }
    }
    if (idx != n) return false;
    if (construct(&z->lit,  lengths,        hlit)  < 0) return false;
    if (construct(&z->dist, lengths + hlit, hdist) < 0) return false;
    return true;
}

bool imgf_inflate_init(imgf_inflate_t *z, imgf_inflate_src_t src) {
    memset(z, 0, sizeof *z);
    z->src = src;
    z->win = (uint8_t *)imgf_alloc(IMGF_INFLATE_WIN_SIZE, 0);
    if (!z->win) return false;

    int cmf = next_byte(z);
    int flg = next_byte(z);
    if (cmf < 0 || flg < 0) return false;
    if ((cmf & 0x0F) != 8) return false;
    if (((cmf << 8) | flg) % 31 != 0) return false;
    if (flg & 0x20) return false;
    return true;
}

void imgf_inflate_deinit(imgf_inflate_t *z) {
    if (z->win) {
        imgf_free(z->win);
        z->win = NULL;
    }
}

size_t imgf_inflate_read(imgf_inflate_t *z, uint8_t *out, size_t n) {
    size_t produced = 0;
    while (produced < n && !z->ended && !z->err) {
        if (!z->block_active) {
            z->bfinal = (int)bits(z, 1);
            int bt = (int)bits(z, 2);
            if (z->eof) { z->ended = 1; break; }
            if (bt == 0) {
                z->bitbuf = 0;
                z->bitcnt = 0;
                int l0 = next_byte(z), l1 = next_byte(z);
                next_byte(z);
                next_byte(z);
                if (l0 < 0 || l1 < 0) { z->ended = 1; break; }
                z->stored_rem = (uint32_t)(l0 | (l1 << 8));
                z->mode = 0;
            } else if (bt == 1) {
                build_fixed(z);
                z->mode = 1;
                z->copy_rem = 0;
            } else if (bt == 2) {
                if (!build_dynamic(z)) { z->err = 1; break; }
                z->mode = 1;
                z->copy_rem = 0;
            } else {
                z->err = 1;
                break;
            }
            z->block_active = 1;
        }

        if (z->mode == 0) {
            while (produced < n && z->stored_rem > 0) {
                int c = next_byte(z);
                if (c < 0) { z->err = 1; break; }
                uint8_t b = (uint8_t)c;
                out[produced++] = b;
                z->win[z->wpos] = b;
                z->wpos = (z->wpos + 1) & IMGF_INFLATE_WIN_MASK;
                z->stored_rem--;
            }
            if (z->stored_rem == 0) {
                z->block_active = 0;
                if (z->bfinal) z->ended = 1;
            }
            continue;
        }

        while (produced < n) {
            if (z->copy_rem > 0) {
                while (produced < n && z->copy_rem > 0) {
                    uint8_t b = z->win[(z->wpos - z->copy_dist) & IMGF_INFLATE_WIN_MASK];
                    out[produced++] = b;
                    z->win[z->wpos] = b;
                    z->wpos = (z->wpos + 1) & IMGF_INFLATE_WIN_MASK;
                    z->copy_rem--;
                }
                if (z->copy_rem > 0) break;
                continue;
            }

            int sym = decode_sym(z, &z->lit);
            if (sym < 0) { z->err = 1; break; }
            if (sym < 256) {
                uint8_t b = (uint8_t)sym;
                out[produced++] = b;
                z->win[z->wpos] = b;
                z->wpos = (z->wpos + 1) & IMGF_INFLATE_WIN_MASK;
            } else if (sym == 256) {
                z->block_active = 0;
                if (z->bfinal) z->ended = 1;
                break;
            } else {
                sym -= 257;
                if (sym >= 29) { z->err = 1; break; }
                int len = kLenBase[sym] + (int)bits(z, kLenExtra[sym]);
                int ds = decode_sym(z, &z->dist);
                if (ds < 0 || ds >= 30) { z->err = 1; break; }
                int dist = kDistBase[ds] + (int)bits(z, kDistExtra[ds]);
                z->copy_dist = dist;
                z->copy_rem  = len;
            }
        }
    }
    return produced;
}
