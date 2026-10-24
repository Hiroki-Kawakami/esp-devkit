/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * VP8L (WebP lossless) decoder, a row at a time.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"
#include "imgf_webpd_internal.h"

#define WINDOW 32768
#define ROOT_BITS 8
#define MAX_CODE_LEN 15
#define NUM_LITERAL 256
#define NUM_LENGTH 24
#define NUM_DIST 40
#define MAX_CACHE_BITS 11
#define MAX_COPY 4096
#define MAX_FAR_DIST (1048576 - 120)
#define ARENA_BLOCK 65536

enum { PREDICTOR = 0, CROSS_COLOR = 1, SUBTRACT_GREEN = 2, COLOR_INDEXING = 3 };
enum { GREEN = 0, RED = 1, BLUE = 2, ALPHA = 3, DIST = 4 };

static const uint8_t kCodeLengthOrder[19] = {
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
};

static const uint8_t kCodeToPlane[120] = {
    0x18, 0x07, 0x17, 0x19, 0x28, 0x06, 0x27, 0x29, 0x16, 0x1a,
    0x26, 0x2a, 0x38, 0x05, 0x37, 0x39, 0x15, 0x1b, 0x36, 0x3a,
    0x25, 0x2b, 0x48, 0x04, 0x47, 0x49, 0x14, 0x1c, 0x35, 0x3b,
    0x46, 0x4a, 0x24, 0x2c, 0x58, 0x45, 0x4b, 0x34, 0x3c, 0x03,
    0x57, 0x59, 0x13, 0x1d, 0x56, 0x5a, 0x23, 0x2d, 0x44, 0x4c,
    0x55, 0x5b, 0x33, 0x3d, 0x68, 0x02, 0x67, 0x69, 0x12, 0x1e,
    0x66, 0x6a, 0x22, 0x2e, 0x54, 0x5c, 0x43, 0x4d, 0x65, 0x6b,
    0x32, 0x3e, 0x78, 0x01, 0x77, 0x79, 0x53, 0x5d, 0x11, 0x1f,
    0x64, 0x6c, 0x42, 0x4e, 0x76, 0x7a, 0x21, 0x2f, 0x75, 0x7b,
    0x31, 0x3f, 0x63, 0x6d, 0x52, 0x5e, 0x00, 0x74, 0x7c, 0x41,
    0x4f, 0x10, 0x20, 0x62, 0x6e, 0x30, 0x73, 0x7d, 0x51, 0x5f,
    0x40, 0x72, 0x7e, 0x61, 0x6f, 0x50, 0x71, 0x7f, 0x60, 0x70,
};

/* --- bit reader ------------------------------------------------------------ */

typedef struct {
    webp_cursor_t  cur;
    const uint8_t *p;
    const uint8_t *e;
    uint64_t       val;
    int            nbits;
    bool           overrun;
} bits_t;

static void br_fill(bits_t *b) {
    while (b->nbits <= 56) {
        if (b->p == b->e) {
            size_t n;
            b->p = webp_cursor_refill(&b->cur, &n);
            b->e = b->p + n;
            if (!n) return;
        }
        b->val |= (uint64_t)*b->p++ << b->nbits;
        b->nbits += 8;
    }
}

/* Reading past the end yields zeros and marks the stream overrun. */
static inline void br_skip(bits_t *b, int n) {
    if (n > b->nbits) {
        b->overrun = true;
        b->val = 0;
        b->nbits = 0;
        return;
    }
    b->val >>= n;
    b->nbits -= n;
}

static inline uint32_t br_bits(bits_t *b, int n) {
    if (b->nbits < n) br_fill(b);
    const uint32_t v = (uint32_t)(b->val & ((1ull << n) - 1));
    br_skip(b, n);
    return v;
}

/* --- prefix codes ---------------------------------------------------------- */

typedef struct {
    uint8_t  bits;
    uint16_t value;
} hcode_t;

typedef struct arena_block {
    struct arena_block *next;
    size_t              used;
    size_t              size;
} arena_block_t;

typedef struct {
    arena_block_t *head;
    uint32_t       caps;
} arena_t;

static hcode_t *arena_alloc(arena_t *a, size_t count) {
    const size_t bytes = count * sizeof(hcode_t);
    arena_block_t *b = a->head;
    if (!b || b->size - b->used < bytes) {
        const size_t size = bytes > ARENA_BLOCK ? bytes : ARENA_BLOCK;
        b = (arena_block_t *)imgf_alloc(sizeof *b + size, a->caps);
        if (!b) return NULL;
        b->next = a->head;
        b->used = 0;
        b->size = size;
        a->head = b;
    }
    hcode_t *p = (hcode_t *)((uint8_t *)(b + 1) + b->used);
    b->used += bytes;
    return p;
}

static void arena_free(arena_t *a) {
    while (a->head) {
        arena_block_t *next = a->head->next;
        imgf_free(a->head);
        a->head = next;
    }
}

static uint32_t reverse_bits(uint32_t code, int len) {
    uint32_t r = 0;
    for (int i = 0; i < len; i++, code >>= 1) r = (r << 1) | (code & 1);
    return r;
}

/* A code with a single used symbol takes no bits; any other code has to be
 * complete. */
static hcode_t *build_table(arena_t *a, const uint8_t *lengths, int n) {
    int count[MAX_CODE_LEN + 1] = {0};
    int single = -1;
    for (int s = 0; s < n; s++) {
        count[lengths[s]]++;
        if (lengths[s]) single = s;
    }
    if (count[0] == n) return NULL;
    if (count[0] == n - 1) {
        hcode_t *t = arena_alloc(a, 1 << ROOT_BITS);
        if (!t) return NULL;
        for (int i = 0; i < 1 << ROOT_BITS; i++) t[i] = (hcode_t){ 0, (uint16_t)single };
        return t;
    }
    uint32_t kraft = 0;
    for (int len = 1; len <= MAX_CODE_LEN; len++) kraft += (uint32_t)count[len] << (MAX_CODE_LEN - len);
    if (kraft != 1u << MAX_CODE_LEN) return NULL;

    uint32_t next[MAX_CODE_LEN + 2];
    next[1] = 0;
    for (int len = 1; len <= MAX_CODE_LEN; len++) next[len + 1] = (next[len] + count[len]) << 1;

    uint8_t sub_bits[1 << ROOT_BITS] = {0};
    uint32_t code[MAX_CODE_LEN + 2];
    memcpy(code, next, sizeof code);
    for (int s = 0; s < n; s++) {
        const int len = lengths[s];
        if (len <= ROOT_BITS) continue;
        const uint32_t rev = reverse_bits(code[len]++, len);
        uint8_t *sb = &sub_bits[rev & ((1 << ROOT_BITS) - 1)];
        if (len - ROOT_BITS > *sb) *sb = (uint8_t)(len - ROOT_BITS);
    }
    size_t total = 1 << ROOT_BITS;
    for (int i = 0; i < 1 << ROOT_BITS; i++) if (sub_bits[i]) total += (size_t)1 << sub_bits[i];

    hcode_t *t = arena_alloc(a, total);
    if (!t) return NULL;
    size_t sub = 1 << ROOT_BITS;
    for (int i = 0; i < 1 << ROOT_BITS; i++) {
        if (!sub_bits[i]) continue;
        t[i] = (hcode_t){ (uint8_t)(ROOT_BITS + sub_bits[i]), (uint16_t)sub };
        sub += (size_t)1 << sub_bits[i];
    }
    memcpy(code, next, sizeof code);
    for (int s = 0; s < n; s++) {
        const int len = lengths[s];
        if (!len) continue;
        const uint32_t rev = reverse_bits(code[len]++, len);
        if (len <= ROOT_BITS) {
            for (uint32_t i = rev; i < 1 << ROOT_BITS; i += 1u << len) {
                t[i] = (hcode_t){ (uint8_t)len, (uint16_t)s };
            }
        } else {
            const hcode_t link = t[rev & ((1 << ROOT_BITS) - 1)];
            const int bits = link.bits - ROOT_BITS;
            hcode_t *st = t + link.value;
            for (uint32_t i = rev >> ROOT_BITS; i < 1u << bits; i += 1u << (len - ROOT_BITS)) {
                st[i] = (hcode_t){ (uint8_t)(len - ROOT_BITS), (uint16_t)s };
            }
        }
    }
    return t;
}

static inline int read_symbol(bits_t *b, const hcode_t *t) {
    if (b->nbits < MAX_CODE_LEN) br_fill(b);
    const uint32_t v = (uint32_t)b->val;
    const hcode_t *e = t + (v & ((1 << ROOT_BITS) - 1));
    if (e->bits > ROOT_BITS) {
        const int sub = e->bits - ROOT_BITS;
        e = t + e->value + ((v >> ROOT_BITS) & ((1u << sub) - 1));
        br_skip(b, ROOT_BITS + e->bits);
    } else {
        br_skip(b, e->bits);
    }
    return e->value;
}

typedef struct {
    const hcode_t *codes[5];
} group_t;

typedef struct {
    int       xs;
    int       ys;
    int       group_bits;
    int       group_xs;
    uint32_t *group_map;
    group_t  *groups;
    uint32_t *cache;
    int       cache_bits;
} entropy_t;

typedef struct {
    int       type;
    int       bits;
    int       xs;
    uint32_t *data;
} transform_t;

struct webp_vp8l {
    webp_source_t *src;
    bits_t         br;
    arena_t        arena;
    imgf_err_t     err;

    int  width;
    int  height;
    bool has_alpha;

    transform_t transforms[4];
    int         num_transforms;
    entropy_t   main;

    uint32_t *ring;
    size_t    ring_size;
    size_t    pos;
    size_t    total;
    int       col;
    int       row;
    int       out_row;

    uint32_t *tmp[2];
    uint32_t *pred;
};

static imgf_err_t stream_error(const webp_vp8l_t *d) {
    return d->br.overrun ? webp_eof_error(d->src) : IMGF_ERR_DECODE;
}

static int subsample(int size, int bits) {
    return (size + (1 << bits) - 1) >> bits;
}

static void entropy_free(entropy_t *e) {
    imgf_free(e->group_map);
    imgf_free(e->groups);
    imgf_free(e->cache);
    memset(e, 0, sizeof *e);
}

static imgf_err_t decode_image(webp_vp8l_t *d, int xs, int ys, bool level0, entropy_t *e,
                               uint32_t **out);

static imgf_err_t read_code(webp_vp8l_t *d, int alphabet, uint8_t *lengths, const hcode_t **out) {
    bits_t *b = &d->br;
    memset(lengths, 0, (size_t)alphabet);
    if (br_bits(b, 1)) {
        const int num = (int)br_bits(b, 1) + 1;
        const int s0 = (int)br_bits(b, br_bits(b, 1) ? 8 : 1);
        if (s0 < alphabet) lengths[s0] = 1;
        if (num == 2) {
            const int s1 = (int)br_bits(b, 8);
            if (s1 < alphabet) lengths[s1] = 1;
        }
    } else {
        uint8_t cl_lengths[19] = {0};
        const int num_codes = (int)br_bits(b, 4) + 4;
        for (int i = 0; i < num_codes; i++) cl_lengths[kCodeLengthOrder[i]] = (uint8_t)br_bits(b, 3);
        arena_block_t *mark = d->arena.head;
        const size_t used = mark ? mark->used : 0;
        const hcode_t *cl = build_table(&d->arena, cl_lengths, 19);
        if (!cl) return b->overrun ? stream_error(d) : IMGF_ERR_DECODE;

        int max_symbol = alphabet;
        if (br_bits(b, 1)) {
            const int nbits = 2 + 2 * (int)br_bits(b, 3);
            max_symbol = 2 + (int)br_bits(b, nbits);
            if (max_symbol > alphabet) return IMGF_ERR_DECODE;
        }
        int prev = 8;
        for (int s = 0; s < alphabet;) {
            if (max_symbol-- == 0) break;
            const int c = read_symbol(b, cl);
            if (c < 16) {
                lengths[s++] = (uint8_t)c;
                if (c) prev = c;
                continue;
            }
            static const uint8_t kExtra[3] = { 2, 3, 7 };
            static const uint8_t kOffset[3] = { 3, 3, 11 };
            const int repeat = (int)br_bits(b, kExtra[c - 16]) + kOffset[c - 16];
            if (s + repeat > alphabet) return IMGF_ERR_DECODE;
            memset(lengths + s, c == 16 ? prev : 0, (size_t)repeat);
            s += repeat;
        }
        if (d->arena.head == mark && mark) mark->used = used;
    }
    if (b->overrun) return stream_error(d);
    *out = build_table(&d->arena, lengths, alphabet);
    return *out ? IMGF_OK : IMGF_ERR_DECODE;
}

static imgf_err_t read_codes(webp_vp8l_t *d, entropy_t *e, int xs, int ys, bool level0) {
    uint32_t *map = NULL;
    int num_groups = 1;
    if (level0 && br_bits(&d->br, 1)) {
        const int bits = 2 + (int)br_bits(&d->br, 3);
        e->group_bits = bits;
        e->group_xs = subsample(xs, bits);
        imgf_err_t err = decode_image(d, e->group_xs, subsample(ys, bits), false, NULL, &map);
        if (err != IMGF_OK) return err;
        e->group_map = map;
        const size_t n = (size_t)e->group_xs * subsample(ys, bits);
        for (size_t i = 0; i < n; i++) {
            map[i] = (map[i] >> 8) & 0xffff;
            if ((int)map[i] >= num_groups) num_groups = (int)map[i] + 1;
        }
    }

    /* Only groups the map uses are kept; the rest are read and dropped. */
    int *slot = (int *)imgf_alloc((size_t)num_groups * sizeof *slot, d->arena.caps);
    if (!slot) return IMGF_ERR_OOM;
    for (int i = 0; i < num_groups; i++) slot[i] = map ? -1 : i;
    int used = map ? 0 : num_groups;
    if (map) {
        const size_t n = (size_t)e->group_xs * subsample(ys, e->group_bits);
        for (size_t i = 0; i < n; i++) {
            if (slot[map[i]] < 0) slot[map[i]] = used++;
            map[i] = (uint32_t)slot[map[i]];
        }
    }
    e->groups = (group_t *)imgf_alloc((size_t)used * sizeof *e->groups, d->arena.caps);
    const int green = NUM_LITERAL + NUM_LENGTH + (e->cache_bits ? 1 << e->cache_bits : 0);
    uint8_t *lengths = (uint8_t *)imgf_alloc((size_t)green, d->arena.caps);
    imgf_err_t err = e->groups && lengths ? IMGF_OK : IMGF_ERR_OOM;
    for (int g = 0; err == IMGF_OK && g < num_groups; g++) {
        arena_block_t *mark = d->arena.head;
        const size_t mark_used = mark ? mark->used : 0;
        for (int k = 0; err == IMGF_OK && k < 5; k++) {
            const int alphabet = k == GREEN ? green : k == DIST ? NUM_DIST : NUM_LITERAL;
            const hcode_t *code;
            err = read_code(d, alphabet, lengths, &code);
            if (err == IMGF_OK && slot[g] >= 0) e->groups[slot[g]].codes[k] = code;
        }
        if (slot[g] < 0 && d->arena.head == mark && mark) mark->used = mark_used;
    }
    imgf_free(lengths);
    imgf_free(slot);
    return err;
}

/* --- entropy-coded pixels -------------------------------------------------- */

static inline void cache_insert(entropy_t *e, uint32_t argb) {
    if (e->cache) e->cache[(argb * 0x1e35a7bdu) >> (32 - e->cache_bits)] = argb;
}

static int prefix_value(bits_t *b, int symbol) {
    if (symbol < 4) return symbol + 1;
    const int extra = (symbol - 2) >> 1;
    const int offset = (2 + (symbol & 1)) << extra;
    return offset + (int)br_bits(b, extra) + 1;
}

static int plane_distance(int xs, int code) {
    if (code > 120) return code - 120;
    const int dc = kCodeToPlane[code - 1];
    const int dist = (dc >> 4) * xs + 8 - (dc & 0xf);
    return dist >= 1 ? dist : 1;
}

/* Decodes pixels into `ring` (`ring_size` pixels, a multiple of the width)
 * until `*pos` reaches `target`; a copy may run past it. */
static imgf_err_t decode_pixels(webp_vp8l_t *d, entropy_t *e, uint32_t *ring, size_t ring_size,
                                size_t total, size_t *pos, int *col, int *row, size_t target) {
    bits_t *b = &d->br;
    size_t p = *pos;
    size_t ri = p % ring_size;
    int x = *col, y = *row;
    const int cache_limit = NUM_LITERAL + NUM_LENGTH + (e->cache ? 1 << e->cache_bits : 0);
    while (p < target) {
        const group_t *g = e->groups;
        if (e->group_map) g += e->group_map[(y >> e->group_bits) * e->group_xs + (x >> e->group_bits)];
        const int code = read_symbol(b, g->codes[GREEN]);
        if (code < NUM_LITERAL) {
            const uint32_t red = (uint32_t)read_symbol(b, g->codes[RED]);
            const uint32_t blue = (uint32_t)read_symbol(b, g->codes[BLUE]);
            const uint32_t alpha = (uint32_t)read_symbol(b, g->codes[ALPHA]);
            const uint32_t argb = (alpha << 24) | (red << 16) | ((uint32_t)code << 8) | blue;
            ring[ri] = argb;
            cache_insert(e, argb);
            p++;
            if (++ri == ring_size) ri = 0;
            if (++x == e->xs) {
                x = 0;
                y++;
            }
        } else if (code < NUM_LITERAL + NUM_LENGTH) {
            const int length = prefix_value(b, code - NUM_LITERAL);
            const int dist_symbol = read_symbol(b, g->codes[DIST]);
            const size_t dist = (size_t)plane_distance(e->xs, prefix_value(b, dist_symbol));
            if (b->overrun) break;
            if (p < dist || total - p < (size_t)length) return IMGF_ERR_DECODE;
            size_t si = ri >= dist ? ri - dist : ri + ring_size - dist;
            for (int i = 0; i < length; i++) {
                const uint32_t argb = ring[si];
                ring[ri] = argb;
                cache_insert(e, argb);
                if (++si == ring_size) si = 0;
                if (++ri == ring_size) ri = 0;
            }
            p += (size_t)length;
            x += length;
            while (x >= e->xs) {
                x -= e->xs;
                y++;
            }
        } else if (code < cache_limit) {
            const uint32_t argb = e->cache[code - NUM_LITERAL - NUM_LENGTH];
            ring[ri] = argb;
            p++;
            if (++ri == ring_size) ri = 0;
            if (++x == e->xs) {
                x = 0;
                y++;
            }
        } else {
            return IMGF_ERR_DECODE;
        }
        if (b->overrun) break;
    }
    *pos = p;
    *col = x;
    *row = y;
    return b->overrun ? stream_error(d) : IMGF_OK;
}

/* --- transforms ------------------------------------------------------------ */

static imgf_err_t read_transform(webp_vp8l_t *d, int *xs, int ys, unsigned *seen) {
    const int type = (int)br_bits(&d->br, 2);
    if (*seen & (1u << type)) return IMGF_ERR_DECODE;
    *seen |= 1u << type;
    transform_t *t = &d->transforms[d->num_transforms++];
    t->type = type;
    t->xs = *xs;
    if (type == PREDICTOR || type == CROSS_COLOR) {
        t->bits = 2 + (int)br_bits(&d->br, 3);
        return decode_image(d, subsample(*xs, t->bits), subsample(ys, t->bits), false, NULL,
                            &t->data);
    }
    if (type != COLOR_INDEXING) return IMGF_OK;

    const int num_colors = (int)br_bits(&d->br, 8) + 1;
    t->bits = num_colors > 16 ? 0 : num_colors > 4 ? 1 : num_colors > 2 ? 2 : 3;
    *xs = subsample(*xs, t->bits);
    uint32_t *colors;
    imgf_err_t err = decode_image(d, num_colors, 1, false, NULL, &colors);
    if (err != IMGF_OK) return err;
    const int size = 1 << (8 >> t->bits);
    t->data = (uint32_t *)imgf_alloc((size_t)size * 4, d->arena.caps);
    if (!t->data) {
        imgf_free(colors);
        return IMGF_ERR_OOM;
    }
    uint8_t *dst = (uint8_t *)t->data;
    const uint8_t *src = (const uint8_t *)colors;
    memcpy(dst, src, 4);
    for (int i = 4; i < 4 * num_colors; i++) dst[i] = (uint8_t)(src[i] + dst[i - 4]);
    memset(dst + 4 * num_colors, 0, (size_t)(4 * (size - num_colors)));
    imgf_free(colors);
    return IMGF_OK;
}

static inline uint32_t add_pixels(uint32_t a, uint32_t b) {
    const uint32_t ag = (a & 0xff00ff00u) + (b & 0xff00ff00u);
    const uint32_t rb = (a & 0x00ff00ffu) + (b & 0x00ff00ffu);
    return (ag & 0xff00ff00u) | (rb & 0x00ff00ffu);
}

static inline uint32_t average2(uint32_t a, uint32_t b) {
    return (((a ^ b) & 0xfefefefeu) >> 1) + (a & b);
}

static inline uint32_t clip255(uint32_t a) {
    return a < 256 ? a : ~a >> 24;
}

static inline int sub3(int a, int b, int c) {
    const int pb = b - c, pa = a - c;
    return (pb < 0 ? -pb : pb) - (pa < 0 ? -pa : pa);
}

static uint32_t select_pred(uint32_t a, uint32_t b, uint32_t c) {
    const int pa_minus_pb = sub3((int)(a >> 24), (int)(b >> 24), (int)(c >> 24)) +
                            sub3((int)((a >> 16) & 0xff), (int)((b >> 16) & 0xff), (int)((c >> 16) & 0xff)) +
                            sub3((int)((a >> 8) & 0xff), (int)((b >> 8) & 0xff), (int)((c >> 8) & 0xff)) +
                            sub3((int)(a & 0xff), (int)(b & 0xff), (int)(c & 0xff));
    return pa_minus_pb <= 0 ? a : b;
}

static uint32_t clamped_add_subtract_full(uint32_t c0, uint32_t c1, uint32_t c2) {
    uint32_t out = 0;
    for (int s = 0; s < 32; s += 8) {
        const int v = (int)((c0 >> s) & 0xff) + (int)((c1 >> s) & 0xff) - (int)((c2 >> s) & 0xff);
        out |= clip255((uint32_t)v) << s;
    }
    return out;
}

static uint32_t clamped_add_subtract_half(uint32_t c0, uint32_t c1, uint32_t c2) {
    const uint32_t ave = average2(c0, c1);
    uint32_t out = 0;
    for (int s = 0; s < 32; s += 8) {
        const int a = (int)((ave >> s) & 0xff);
        const int b = (int)((c2 >> s) & 0xff);
        out |= clip255((uint32_t)(a + (a - b) / 2)) << s;
    }
    return out;
}

static uint32_t predict(int mode, uint32_t left, const uint32_t *top) {
    switch (mode) {
    case 1: return left;
    case 2: return top[0];
    case 3: return top[1];
    case 4: return top[-1];
    case 5: return average2(average2(left, top[1]), top[0]);
    case 6: return average2(left, top[-1]);
    case 7: return average2(left, top[0]);
    case 8: return average2(top[-1], top[0]);
    case 9: return average2(top[0], top[1]);
    case 10: return average2(average2(left, top[-1]), average2(top[0], top[1]));
    case 11: return select_pred(top[0], left, top[-1]);
    case 12: return clamped_add_subtract_full(left, top[0], top[-1]);
    case 13: return clamped_add_subtract_half(left, top[0], top[-1]);
    default: return 0xff000000u;
    }
}

/* `d->pred` holds the previous output row followed by the current one, so the
 * top-right of the last pixel is the first pixel of the current row, as the
 * format defines it. */
static void inverse_predictor(webp_vp8l_t *d, const transform_t *t, int y, const uint32_t *in,
                              uint32_t *out) {
    const int w = t->xs;
    uint32_t *prev = d->pred;
    uint32_t *cur = d->pred + w;
    if (y == 0) {
        cur[0] = add_pixels(in[0], 0xff000000u);
        for (int x = 1; x < w; x++) cur[x] = add_pixels(in[x], cur[x - 1]);
    } else {
        const uint32_t *modes = t->data + (y >> t->bits) * subsample(w, t->bits);
        cur[0] = add_pixels(in[0], prev[0]);
        for (int x = 1; x < w; x++) {
            const int mode = (int)((modes[x >> t->bits] >> 8) & 0xf);
            cur[x] = add_pixels(in[x], predict(mode, cur[x - 1], prev + x));
        }
    }
    memcpy(out, cur, (size_t)w * 4);
    memcpy(prev, cur, (size_t)w * 4);
}

static inline int color_delta(int8_t pred, int8_t color) {
    return ((int)pred * color) >> 5;
}

static void inverse_cross_color(const transform_t *t, int y, const uint32_t *in, uint32_t *out) {
    const uint32_t *codes = t->data + (y >> t->bits) * subsample(t->xs, t->bits);
    for (int x = 0; x < t->xs; x++) {
        const uint32_t code = codes[x >> t->bits];
        const uint32_t argb = in[x];
        const int8_t green = (int8_t)(argb >> 8);
        int red = (int)((argb >> 16) & 0xff);
        int blue = (int)(argb & 0xff);
        red += color_delta((int8_t)code, green);
        red &= 0xff;
        blue += color_delta((int8_t)(code >> 8), green);
        blue += color_delta((int8_t)(code >> 16), (int8_t)red);
        blue &= 0xff;
        out[x] = (argb & 0xff00ff00u) | ((uint32_t)red << 16) | (uint32_t)blue;
    }
}

static void add_green(const transform_t *t, const uint32_t *in, uint32_t *out) {
    for (int x = 0; x < t->xs; x++) {
        const uint32_t argb = in[x];
        const uint32_t green = (argb >> 8) & 0xff;
        const uint32_t rb = ((argb & 0x00ff00ffu) + ((green << 16) | green)) & 0x00ff00ffu;
        out[x] = (argb & 0xff00ff00u) | rb;
    }
}

static void inverse_color_indexing(const transform_t *t, const uint32_t *in, uint32_t *out) {
    const int bpp = 8 >> t->bits;
    if (bpp == 8) {
        for (int x = 0; x < t->xs; x++) out[x] = t->data[(in[x] >> 8) & 0xff];
        return;
    }
    const uint32_t mask = (1u << bpp) - 1;
    const int per_byte_mask = (1 << t->bits) - 1;
    uint32_t packed = 0;
    for (int x = 0; x < t->xs; x++) {
        if ((x & per_byte_mask) == 0) packed = (*in++ >> 8) & 0xff;
        out[x] = t->data[packed & mask];
        packed >>= bpp;
    }
}

static imgf_err_t read_entropy(webp_vp8l_t *d, entropy_t *e, int xs, int ys, bool level0) {
    e->xs = xs;
    e->ys = ys;
    if (br_bits(&d->br, 1)) {
        e->cache_bits = (int)br_bits(&d->br, 4);
        if (e->cache_bits < 1 || e->cache_bits > MAX_CACHE_BITS) return IMGF_ERR_DECODE;
    }
    imgf_err_t err = read_codes(d, e, xs, ys, level0);
    if (err != IMGF_OK) return err;
    if (e->cache_bits) {
        e->cache = (uint32_t *)imgf_alloc((size_t)4 << e->cache_bits, d->arena.caps);
        if (!e->cache) return IMGF_ERR_OOM;
        memset(e->cache, 0, (size_t)4 << e->cache_bits);
    }
    return d->br.overrun ? stream_error(d) : IMGF_OK;
}

/* A sub-image is decoded whole into `*out`; the level-0 image only has its
 * transforms and codes read here, into `main`. */
static imgf_err_t decode_image(webp_vp8l_t *d, int xs, int ys, bool level0, entropy_t *main,
                               uint32_t **out) {
    if (level0) {
        unsigned seen = 0;
        while (br_bits(&d->br, 1)) {
            imgf_err_t err = read_transform(d, &xs, ys, &seen);
            if (err != IMGF_OK) return err;
        }
        return read_entropy(d, main, xs, ys, true);
    }

    entropy_t e = {0};
    imgf_err_t err = read_entropy(d, &e, xs, ys, false);
    const size_t total = (size_t)xs * ys;
    uint32_t *data = NULL;
    if (err == IMGF_OK) {
        data = (uint32_t *)imgf_alloc(total * 4, d->arena.caps);
        if (!data) err = IMGF_ERR_OOM;
    }
    if (err == IMGF_OK) {
        size_t pos = 0;
        int col = 0, row = 0;
        err = decode_pixels(d, &e, data, total, total, &pos, &col, &row, total);
    }
    entropy_free(&e);
    if (err != IMGF_OK) {
        imgf_free(data);
        return err;
    }
    *out = data;
    return IMGF_OK;
}

imgf_err_t webp_vp8l_next_row(webp_vp8l_t *d, uint32_t *argb) {
    if (d->err != IMGF_OK) return d->err;
    if (d->out_row >= d->height) return IMGF_ERR_INVALID_STATE;
    const int xs = d->main.xs;
    const size_t end = (size_t)(d->out_row + 1) * xs;
    if (d->pos < end) {
        d->err = decode_pixels(d, &d->main, d->ring, d->ring_size, d->total, &d->pos, &d->col,
                               &d->row, end);
        if (d->err != IMGF_OK) return d->err;
    }
    const uint32_t *in = d->ring + ((size_t)d->out_row * xs) % d->ring_size;
    for (int i = d->num_transforms - 1; i >= 0; i--) {
        const transform_t *t = &d->transforms[i];
        uint32_t *o = i == 0 ? argb : d->tmp[i & 1];
        switch (t->type) {
        case PREDICTOR: inverse_predictor(d, t, d->out_row, in, o); break;
        case CROSS_COLOR: inverse_cross_color(t, d->out_row, in, o); break;
        case SUBTRACT_GREEN: add_green(t, in, o); break;
        default: inverse_color_indexing(t, in, o); break;
        }
        in = o;
    }
    if (d->num_transforms == 0) memcpy(argb, in, (size_t)d->width * 4);
    d->out_row++;
    return IMGF_OK;
}

imgf_err_t webp_vp8l_open(webp_vp8l_t **out, webp_source_t *src, size_t pos, size_t len,
                          int width, int height, uint32_t caps) {
    webp_vp8l_t *d = (webp_vp8l_t *)calloc(1, sizeof *d);
    *out = d;
    if (!d) return IMGF_ERR_OOM;
    d->src = src;
    d->arena.caps = caps;
    if (!webp_cursor_init(&d->br.cur, src, pos, len, WINDOW, caps)) return IMGF_ERR_OOM;

    if (width == 0) {
        if (br_bits(&d->br, 8) != 0x2f) return stream_error(d);
        width = (int)br_bits(&d->br, 14) + 1;
        height = (int)br_bits(&d->br, 14) + 1;
        d->has_alpha = br_bits(&d->br, 1);
        if (br_bits(&d->br, 3) != 0) return stream_error(d);
    }
    d->width = width;
    d->height = height;

    imgf_err_t err = decode_image(d, width, height, true, &d->main, NULL);
    if (err != IMGF_OK) return err;

    const size_t xs = (size_t)d->main.xs;
    d->total = xs * (size_t)height;
    size_t need = 7 * xs + 8 > MAX_FAR_DIST ? 7 * xs + 8 : MAX_FAR_DIST;
    if (need < xs + MAX_COPY) need = xs + MAX_COPY;
    need = (need + xs - 1) / xs * xs;
    d->ring_size = need < d->total ? need : d->total;
    d->ring = (uint32_t *)imgf_alloc(d->ring_size * 4, caps);
    d->tmp[0] = (uint32_t *)imgf_alloc((size_t)width * 4, caps);
    d->tmp[1] = (uint32_t *)imgf_alloc((size_t)width * 4, caps);
    d->pred = (uint32_t *)imgf_alloc((size_t)width * 8, caps);
    if (!d->ring || !d->tmp[0] || !d->tmp[1] || !d->pred) return IMGF_ERR_OOM;
    return IMGF_OK;
}

int webp_vp8l_width(const webp_vp8l_t *d) { return d->width; }
int webp_vp8l_height(const webp_vp8l_t *d) { return d->height; }
bool webp_vp8l_has_alpha(const webp_vp8l_t *d) { return d->has_alpha; }

void webp_vp8l_destroy(webp_vp8l_t *d) {
    if (!d) return;
    webp_cursor_deinit(&d->br.cur);
    for (int i = 0; i < d->num_transforms; i++) imgf_free(d->transforms[i].data);
    entropy_free(&d->main);
    arena_free(&d->arena);
    imgf_free(d->ring);
    imgf_free(d->tmp[0]);
    imgf_free(d->tmp[1]);
    imgf_free(d->pred);
    free(d);
}
