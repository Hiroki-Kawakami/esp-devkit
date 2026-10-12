/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "pb.h"
#include <stdlib.h>
#include <string.h>

#define WT_VARINT 0
#define WT_I64    1
#define WT_LEN    2
#define WT_I32    5

#define MAX_DEPTH 16
#define ALIGN(n)  (((n) + 7) & ~(size_t)7)

typedef struct {
    void *owner;
} block_t;

typedef struct {
    uint8_t *cur;
    uint8_t *end;
} arena_t;

typedef struct {
    uint32_t number;
    uint8_t wt;
    const uint8_t *val;
    size_t len;
} wire_field_t;

static size_t elem_size(uint8_t type) {
    switch (type) {
    case PB_TYPE_INT32:
    case PB_TYPE_UINT32:
        return 4;
    case PB_TYPE_INT64:
    case PB_TYPE_UINT64:
        return 8;
    case PB_TYPE_BOOL:
        return sizeof(bool);
    case PB_TYPE_BYTES:
        return sizeof(pb_bytes_t);
    default:
        return sizeof(void *);
    }
}

static bool is_scalar(uint8_t type) {
    return type != PB_TYPE_BYTES && type != PB_TYPE_MESSAGE;
}

static uint64_t get_scalar(uint8_t type, const uint8_t *p) {
    switch (type) {
    case PB_TYPE_INT32:
        return (uint64_t)(int64_t)*(const int32_t *)p;
    case PB_TYPE_UINT32:
        return *(const uint32_t *)p;
    case PB_TYPE_BOOL:
        return *(const bool *)p;
    default:
        return *(const uint64_t *)p;
    }
}

static void set_scalar(uint8_t type, uint8_t *p, uint64_t v) {
    switch (type) {
    case PB_TYPE_INT32:
    case PB_TYPE_UINT32:
        *(uint32_t *)p = (uint32_t)v;
        break;
    case PB_TYPE_BOOL:
        *(bool *)p = v != 0;
        break;
    default:
        *(uint64_t *)p = v;
        break;
    }
}

static size_t varint_size(uint64_t v) {
    size_t n = 1;
    for (; v >= 0x80; v >>= 7) n++;
    return n;
}

// out == NULL only measures.
static size_t put_varint(uint8_t *out, uint64_t v) {
    if (!out) return varint_size(v);
    size_t n = 0;
    for (; v >= 0x80; v >>= 7) out[n++] = (uint8_t)v | 0x80;
    out[n++] = (uint8_t)v;
    return n;
}

static uint8_t *at(uint8_t *out, size_t n) {
    return out ? out + n : NULL;
}

static size_t pack_msg(const pb_msgdesc_t *desc, const uint8_t *msg, uint8_t *out);

static size_t pack_len(const pb_field_t *f, size_t len, uint8_t *out) {
    size_t n = put_varint(out, (uint64_t)f->number << 3 | WT_LEN);
    return n + put_varint(at(out, n), len);
}

static size_t pack_value(const pb_field_t *f, const uint8_t *p, uint8_t *out) {
    if (f->type == PB_TYPE_BYTES) {
        const pb_bytes_t *b = (const pb_bytes_t *)p;
        size_t n = pack_len(f, b->len, out);
        if (out && b->len) memcpy(out + n, b->data, b->len);
        return n + b->len;
    }
    if (f->type == PB_TYPE_MESSAGE) {
        const uint8_t *sub = *(const uint8_t *const *)p;
        size_t len = sub ? pack_msg(f->msg, sub, NULL) : 0;
        size_t n = pack_len(f, len, out);
        if (out && sub) pack_msg(f->msg, sub, out + n);
        return n + len;
    }
    size_t n = put_varint(out, (uint64_t)f->number << 3 | WT_VARINT);
    return n + put_varint(at(out, n), get_scalar(f->type, p));
}

static bool is_default(const pb_field_t *f, const uint8_t *p) {
    if (f->type == PB_TYPE_BYTES) return ((const pb_bytes_t *)p)->len == 0;
    if (f->type == PB_TYPE_MESSAGE) return *(const void *const *)p == NULL;
    return get_scalar(f->type, p) == 0;
}

static size_t pack_msg(const pb_msgdesc_t *desc, const uint8_t *msg, uint8_t *out) {
    size_t n = 0;
    for (size_t i = 0; i < desc->n_fields; i++) {
        const pb_field_t *f = &desc->fields[i];
        const uint8_t *p = msg + f->offset;
        if (f->flags & PB_FLAG_REPEATED) {
            size_t count = *(const size_t *)(msg + f->qoffset);
            const uint8_t *arr = *(const uint8_t *const *)p;
            size_t es = elem_size(f->type);
            if (!count) continue;
            if (!is_scalar(f->type)) {
                for (size_t k = 0; k < count; k++) n += pack_value(f, arr + k * es, at(out, n));
                continue;
            }
            size_t len = 0;
            for (size_t k = 0; k < count; k++) len += varint_size(get_scalar(f->type, arr + k * es));
            n += pack_len(f, len, at(out, n));
            for (size_t k = 0; k < count; k++) n += put_varint(at(out, n), get_scalar(f->type, arr + k * es));
        } else if (f->flags & PB_FLAG_ONEOF) {
            if (*(const uint32_t *)(msg + f->qoffset) == f->number) n += pack_value(f, p, at(out, n));
        } else if (!is_default(f, p)) {
            n += pack_value(f, p, at(out, n));
        }
    }
    return n;
}

size_t pb_packed_size(const pb_msgdesc_t *desc, const void *msg) {
    return pack_msg(desc, msg, NULL);
}

size_t pb_pack(const pb_msgdesc_t *desc, const void *msg, uint8_t *out) {
    return pack_msg(desc, msg, out);
}

static bool get_varint(const uint8_t **p, const uint8_t *end, uint64_t *v) {
    uint64_t r = 0;
    for (unsigned shift = 0; shift < 64 && *p < end; shift += 7) {
        uint8_t b = *(*p)++;
        r |= (uint64_t)(b & 0x7f) << shift;
        if (!(b & 0x80)) {
            *v = r;
            return true;
        }
    }
    return false;
}

static bool next_field(const uint8_t **p, const uint8_t *end, wire_field_t *w) {
    uint64_t tag;
    if (!get_varint(p, end, &tag) || tag >> 3 == 0 || tag >> 3 > UINT32_MAX) return false;
    w->number = (uint32_t)(tag >> 3);
    w->wt = tag & 7;
    w->val = *p;
    uint64_t v;
    switch (w->wt) {
    case WT_VARINT:
        if (!get_varint(p, end, &v)) return false;
        w->len = *p - w->val;
        return true;
    case WT_I64:
        w->len = 8;
        break;
    case WT_I32:
        w->len = 4;
        break;
    case WT_LEN:
        if (!get_varint(p, end, &v) || v > (uint64_t)(end - *p)) return false;
        w->val = *p;
        w->len = v;
        break;
    default:
        return false;
    }
    if (w->len > (size_t)(end - *p)) return false;
    *p += w->len;
    return true;
}

static const pb_field_t *find_field(const pb_msgdesc_t *desc, uint32_t number) {
    size_t lo = 0, hi = desc->n_fields;
    while (lo < hi) {
        size_t mid = (lo + hi) / 2;
        uint32_t n = desc->fields[mid].number;
        if (n == number) return &desc->fields[mid];
        if (n < number) lo = mid + 1;
        else hi = mid;
    }
    return NULL;
}

static size_t count_varints(const uint8_t *p, size_t len) {
    size_t n = 0;
    for (size_t i = 0; i < len; i++) n += !(p[i] & 0x80);
    return n;
}

static bool is_packed(const pb_field_t *f, const wire_field_t *w) {
    return is_scalar(f->type) && w->wt == WT_LEN;
}

// Upper bound of the arena the decoded fields of one message need.
static bool measure(const pb_msgdesc_t *desc, const uint8_t *p, const uint8_t *end, int depth, size_t *need) {
    if (depth > MAX_DEPTH) return false;
    wire_field_t w;
    while (p < end) {
        if (!next_field(&p, end, &w)) return false;
        const pb_field_t *f = find_field(desc, w.number);
        if (!f) continue;
        if (f->flags & PB_FLAG_REPEATED) {
            size_t count = is_packed(f, &w) ? count_varints(w.val, w.len) : 1;
            *need += count * elem_size(f->type) + 7;
        }
        if (f->type == PB_TYPE_MESSAGE && w.wt == WT_LEN) {
            *need += ALIGN(f->msg->size);
            if (!measure(f->msg, w.val, w.val + w.len, depth + 1, need)) return false;
        }
    }
    return true;
}

static uint8_t *arena_alloc(arena_t *a, size_t size) {
    size = ALIGN(size);
    if (size > (size_t)(a->end - a->cur)) return NULL;
    uint8_t *p = a->cur;
    a->cur += size;
    return p;
}

static bool decode(const pb_msgdesc_t *desc, const uint8_t *data, const uint8_t *end, uint8_t *msg, arena_t *a);

static bool decode_value(const pb_field_t *f, const wire_field_t *w, uint8_t *dst, arena_t *a) {
    if (f->type == PB_TYPE_BYTES) {
        if (w->wt != WT_LEN) return false;
        *(pb_bytes_t *)dst = (pb_bytes_t){w->len, w->len ? (uint8_t *)w->val : NULL};
        return true;
    }
    if (f->type == PB_TYPE_MESSAGE) {
        if (w->wt != WT_LEN) return false;
        uint8_t *sub = arena_alloc(a, f->msg->size);
        if (!sub) return false;
        *(uint8_t **)dst = sub;
        return decode(f->msg, w->val, w->val + w->len, sub, a);
    }
    uint64_t v;
    const uint8_t *p = w->val;
    if (w->wt != WT_VARINT || !get_varint(&p, w->val + w->len, &v)) return false;
    set_scalar(f->type, dst, v);
    return true;
}

static bool decode(const pb_msgdesc_t *desc, const uint8_t *data, const uint8_t *end, uint8_t *msg, arena_t *a) {
    wire_field_t w;
    const uint8_t *p = data;
    while (p < end) {
        if (!next_field(&p, end, &w)) return false;
        const pb_field_t *f = find_field(desc, w.number);
        if (f && f->flags & PB_FLAG_REPEATED) {
            *(size_t *)(msg + f->qoffset) += is_packed(f, &w) ? count_varints(w.val, w.len) : 1;
        }
    }
    for (size_t i = 0; i < desc->n_fields; i++) {
        const pb_field_t *f = &desc->fields[i];
        if (!(f->flags & PB_FLAG_REPEATED)) continue;
        size_t *count = (size_t *)(msg + f->qoffset);
        if (!*count) continue;
        uint8_t *arr = arena_alloc(a, *count * elem_size(f->type));
        if (!arr) return false;
        *(uint8_t **)(msg + f->offset) = arr;
        *count = 0;
    }

    p = data;
    while (p < end) {
        next_field(&p, end, &w);
        const pb_field_t *f = find_field(desc, w.number);
        if (!f) continue;
        if (!(f->flags & PB_FLAG_REPEATED)) {
            if (f->flags & PB_FLAG_ONEOF) *(uint32_t *)(msg + f->qoffset) = f->number;
            if (!decode_value(f, &w, msg + f->offset, a)) return false;
            continue;
        }
        uint8_t *arr = *(uint8_t **)(msg + f->offset);
        size_t *count = (size_t *)(msg + f->qoffset);
        size_t es = elem_size(f->type);
        if (!is_packed(f, &w)) {
            if (!decode_value(f, &w, arr + (*count)++ * es, a)) return false;
            continue;
        }
        const uint8_t *q = w.val;
        const uint8_t *q_end = w.val + w.len;
        while (q < q_end) {
            uint64_t v;
            if (!get_varint(&q, q_end, &v)) return false;
            set_scalar(f->type, arr + (*count)++ * es, v);
        }
    }
    return true;
}

void *pb_unpack(const pb_msgdesc_t *desc, void *owner, const uint8_t *data, size_t len) {
    size_t need = ALIGN(sizeof(block_t)) + ALIGN(desc->size);
    block_t *block = NULL;
    if (measure(desc, data, data + len, 0, &need)) block = calloc(1, need);
    if (block) {
        block->owner = owner;
        uint8_t *msg = (uint8_t *)block + ALIGN(sizeof(block_t));
        arena_t a = {msg + ALIGN(desc->size), (uint8_t *)block + need};
        if (decode(desc, data, data + len, msg, &a)) return msg;
        free(block);
    }
    free(owner);
    return NULL;
}

void pb_free(void *msg) {
    if (!msg) return;
    block_t *block = (block_t *)((uint8_t *)msg - ALIGN(sizeof(block_t)));
    free(block->owner);
    free(block);
}
