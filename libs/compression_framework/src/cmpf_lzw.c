/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "cmpf_lzw.h"

#include <string.h>

#define NO_CODE 0xFFFFu

static int next_byte(cmpf_lzw_t *z) {
    if (z->in_avail == 0) {
        z->in_ptr = z->src.refill(z->src.user, &z->in_avail);
        if (z->in_avail == 0) return -1;
    }
    z->in_avail--;
    return *z->in_ptr++;
}

static int next_code(cmpf_lzw_t *z) {
    while (z->bitcnt < z->width) {
        int c = next_byte(z);
        if (c < 0) return -1;
        if (z->msb_first) {
            z->bitbuf = (z->bitbuf << 8) | (uint32_t)c;
        } else {
            z->bitbuf |= (uint32_t)c << z->bitcnt;
        }
        z->bitcnt += 8;
    }
    const uint32_t mask = (1u << z->width) - 1;
    uint32_t code;
    z->bitcnt -= z->width;
    if (z->msb_first) {
        code = (z->bitbuf >> z->bitcnt) & mask;
        z->bitbuf &= (1u << z->bitcnt) - 1;
    } else {
        code = z->bitbuf & mask;
        z->bitbuf >>= z->width;
    }
    return (int)code;
}

static void restart_table(cmpf_lzw_t *z) {
    z->width = (uint8_t)(z->literal_bits + 1);
    z->next = (uint16_t)(z->clear + 2);
    z->prev = NO_CODE;
}

bool cmpf_lzw_init(cmpf_lzw_t *z, cmpf_src_t src, const cmpf_lzw_opts_t *opts, void *work) {
    memset(z, 0, sizeof *z);
    if (!work || !opts || opts->literal_bits < 2 || opts->literal_bits > 8) return false;
    z->src = src;
    z->prefix = (uint16_t *)work;
    z->suffix = (uint8_t *)(z->prefix + CMPF_LZW_MAX_CODES);
    z->stack = z->suffix + CMPF_LZW_MAX_CODES;
    z->literal_bits = opts->literal_bits;
    z->msb_first = opts->msb_first;
    z->early_change = opts->early_change;
    z->clear = (uint16_t)(1u << z->literal_bits);
    for (uint16_t i = 0; i < z->clear; i++) z->suffix[i] = (uint8_t)i;
    restart_table(z);
    return true;
}

size_t cmpf_lzw_read(cmpf_lzw_t *z, uint8_t *out, size_t n) {
    size_t produced = 0;
    for (;;) {
        while (produced < n && z->stack_len) out[produced++] = z->stack[--z->stack_len];
        if (produced == n || z->ended || z->err) break;

        int code = next_code(z);
        if (code < 0 || code == z->clear + 1) {
            z->ended = 1;
            break;
        }
        if (code == z->clear) {
            restart_table(z);
            continue;
        }
        if (z->prev == NO_CODE) {
            if (code > z->clear) {
                z->err = 1;
                break;
            }
            z->stack[z->stack_len++] = (uint8_t)code;
            z->prev = (uint16_t)code;
            z->prev_first = (uint8_t)code;
            continue;
        }

        uint32_t c;
        if (code < z->next) {
            c = (uint32_t)code;
        } else if (code == z->next && z->next < CMPF_LZW_MAX_CODES) {
            z->stack[z->stack_len++] = z->prev_first;
            c = z->prev;
        } else {
            z->err = 1;
            break;
        }
        while (c > z->clear) {
            z->stack[z->stack_len++] = z->suffix[c];
            c = z->prefix[c];
        }
        const uint8_t first = (uint8_t)c;
        z->stack[z->stack_len++] = first;

        if (z->next < CMPF_LZW_MAX_CODES) {
            z->prefix[z->next] = z->prev;
            z->suffix[z->next] = first;
            z->next++;
            if (z->width < 12 && z->next + z->early_change >= (1u << z->width)) z->width++;
        }
        z->prev = (uint16_t)code;
        z->prev_first = first;
    }
    return produced;
}
