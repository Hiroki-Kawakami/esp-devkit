/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "pbcheck.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define FILL_DEPTH     6
#define FILL_MESSAGES  64
#define MAX_FAILURES   10
#define UNKNOWN_NUMBER 100000

static uint64_t s_rng = 0x9E3779B97F4A7C15ull;
static unsigned s_budget;

static uint64_t rnd(void) {
    s_rng ^= s_rng << 13;
    s_rng ^= s_rng >> 7;
    s_rng ^= s_rng << 17;
    return s_rng;
}

static unsigned pick(unsigned n) {
    return (unsigned)(rnd() % n);
}

static uint64_t rnd_value(void) {
    switch (pick(6)) {
    case 0:
        return 0;
    case 1:
        return pick(128);
    case 2:
        return (uint64_t)-(int64_t)pick(1000);
    case 3:
        return UINT64_MAX;
    case 4:
        return 1ull << pick(64);
    default:
        return rnd();
    }
}

static size_t ref_elem_size(ProtobufCType type) {
    switch (type) {
    case PROTOBUF_C_TYPE_INT64:
    case PROTOBUF_C_TYPE_UINT64:
        return 8;
    case PROTOBUF_C_TYPE_BYTES:
        return sizeof(ProtobufCBinaryData);
    case PROTOBUF_C_TYPE_MESSAGE:
        return sizeof(void *);
    default:
        return 4;
    }
}

static size_t elem_size(uint8_t type) {
    switch (type) {
    case PB_TYPE_INT64:
    case PB_TYPE_UINT64:
        return 8;
    case PB_TYPE_BOOL:
        return sizeof(bool);
    case PB_TYPE_BYTES:
        return sizeof(pb_bytes_t);
    case PB_TYPE_MESSAGE:
        return sizeof(void *);
    default:
        return 4;
    }
}

static bool same_type(ProtobufCType ref, uint8_t type) {
    switch (ref) {
    case PROTOBUF_C_TYPE_INT32:
    case PROTOBUF_C_TYPE_ENUM:
        return type == PB_TYPE_INT32;
    case PROTOBUF_C_TYPE_INT64:
        return type == PB_TYPE_INT64;
    case PROTOBUF_C_TYPE_UINT32:
        return type == PB_TYPE_UINT32;
    case PROTOBUF_C_TYPE_UINT64:
        return type == PB_TYPE_UINT64;
    case PROTOBUF_C_TYPE_BOOL:
        return type == PB_TYPE_BOOL;
    case PROTOBUF_C_TYPE_BYTES:
        return type == PB_TYPE_BYTES;
    case PROTOBUF_C_TYPE_MESSAGE:
        return type == PB_TYPE_MESSAGE;
    default:
        return false;
    }
}

static ProtobufCMessage *fill(const ProtobufCMessageDescriptor *d, int depth);

// Oneof members and repeated elements always get a message: protobuf-c packs a
// NULL one as empty, which decodes back as non-NULL.
static void fill_value(const ProtobufCFieldDescriptor *f, uint8_t *p, int depth, bool need_msg) {
    switch (f->type) {
    case PROTOBUF_C_TYPE_ENUM: {
        const ProtobufCEnumDescriptor *e = f->descriptor;
        *(int32_t *)p = pick(2) ? e->values[pick(e->n_values)].value : (int32_t)rnd_value();
        break;
    }
    case PROTOBUF_C_TYPE_INT64:
    case PROTOBUF_C_TYPE_UINT64:
        *(uint64_t *)p = rnd_value();
        break;
    case PROTOBUF_C_TYPE_BOOL:
        *(protobuf_c_boolean *)p = pick(2);
        break;
    case PROTOBUF_C_TYPE_BYTES: {
        ProtobufCBinaryData *b = (ProtobufCBinaryData *)p;
        b->len = pick(4) ? pick(24) : 0;
        b->data = b->len ? malloc(b->len) : NULL;
        for (size_t i = 0; i < b->len; i++) b->data[i] = (uint8_t)rnd();
        break;
    }
    case PROTOBUF_C_TYPE_MESSAGE:
        *(void **)p = need_msg || (depth < FILL_DEPTH && pick(2)) ? fill(f->descriptor, depth + 1) : NULL;
        break;
    default:
        *(uint32_t *)p = (uint32_t)rnd_value();
        break;
    }
}

static ProtobufCMessage *fill(const ProtobufCMessageDescriptor *d, int depth) {
    ProtobufCMessage *m = malloc(d->sizeof_message);
    d->message_init(m);
    if (depth >= FILL_DEPTH || s_budget == 0) return m;
    s_budget--;
    uint8_t *base = (uint8_t *)m;
    for (unsigned i = 0; i < d->n_fields; i++) {
        const ProtobufCFieldDescriptor *f = &d->fields[i];
        if (f->flags & PROTOBUF_C_FIELD_FLAG_ONEOF) {
            bool first = true;
            for (unsigned j = 0; j < i; j++) {
                if ((d->fields[j].flags & PROTOBUF_C_FIELD_FLAG_ONEOF) &&
                    d->fields[j].quantifier_offset == f->quantifier_offset) {
                    first = false;
                }
            }
            if (!first) continue;
            unsigned members = 0;
            for (unsigned j = i; j < d->n_fields; j++) {
                members += (d->fields[j].flags & PROTOBUF_C_FIELD_FLAG_ONEOF) &&
                           d->fields[j].quantifier_offset == f->quantifier_offset;
            }
            unsigned k = pick(members + 1);
            for (unsigned j = i; j < d->n_fields && k < members; j++) {
                const ProtobufCFieldDescriptor *g = &d->fields[j];
                if (!(g->flags & PROTOBUF_C_FIELD_FLAG_ONEOF) || g->quantifier_offset != f->quantifier_offset) continue;
                if (k-- == 0) {
                    *(uint32_t *)(base + g->quantifier_offset) = g->id;
                    fill_value(g, base + g->offset, depth, true);
                    break;
                }
            }
        } else if (f->label == PROTOBUF_C_LABEL_REPEATED) {
            size_t n = pick(4);
            size_t es = ref_elem_size(f->type);
            uint8_t *arr = n ? calloc(n, es) : NULL;
            *(size_t *)(base + f->quantifier_offset) = n;
            *(uint8_t **)(base + f->offset) = arr;
            for (size_t k = 0; k < n; k++) fill_value(f, arr + k * es, depth, true);
        } else {
            fill_value(f, base + f->offset, depth, false);
        }
    }
    return m;
}

static bool equal_value(const ProtobufCFieldDescriptor *rf, const uint8_t *e, const pb_field_t *f, const uint8_t *a) {
    switch (rf->type) {
    case PROTOBUF_C_TYPE_INT64:
    case PROTOBUF_C_TYPE_UINT64:
        return *(const uint64_t *)e == *(const uint64_t *)a;
    case PROTOBUF_C_TYPE_BOOL:
        return (*(const protobuf_c_boolean *)e != 0) == *(const bool *)a;
    case PROTOBUF_C_TYPE_BYTES: {
        const ProtobufCBinaryData *x = (const ProtobufCBinaryData *)e;
        const pb_bytes_t *y = (const pb_bytes_t *)a;
        return x->len == y->len && (!x->len || memcmp(x->data, y->data, x->len) == 0);
    }
    case PROTOBUF_C_TYPE_MESSAGE: {
        const void *x = *(const void *const *)e;
        const void *y = *(const void *const *)a;
        if (!x || !y) return x == y;
        return pbcheck_equal(rf->descriptor, x, f->msg, y);
    }
    default:
        return *(const uint32_t *)e == *(const uint32_t *)a;
    }
}

bool pbcheck_equal(const ProtobufCMessageDescriptor *ref, const void *expect, const pb_msgdesc_t *desc,
                   const void *actual) {
    const uint8_t *e = expect;
    const uint8_t *a = actual;
    if (ref->n_fields != desc->n_fields) {
        fprintf(stderr, "%s: %u fields, pb has %u\n", ref->name, ref->n_fields, desc->n_fields);
        return false;
    }
    for (unsigned i = 0; i < ref->n_fields; i++) {
        const ProtobufCFieldDescriptor *rf = &ref->fields[i];
        const pb_field_t *f = &desc->fields[i];
        if (rf->id != f->number || !same_type(rf->type, f->type) ||
            (rf->label == PROTOBUF_C_LABEL_REPEATED) != !!(f->flags & PB_FLAG_REPEATED) ||
            !!(rf->flags & PROTOBUF_C_FIELD_FLAG_ONEOF) != !!(f->flags & PB_FLAG_ONEOF)) {
            fprintf(stderr, "%s.%s: descriptor mismatch\n", ref->name, rf->name);
            return false;
        }
        bool ok;
        if (rf->label == PROTOBUF_C_LABEL_REPEATED) {
            size_t n = *(const size_t *)(e + rf->quantifier_offset);
            ok = n == *(const size_t *)(a + f->qoffset);
            const uint8_t *x = *(const uint8_t *const *)(e + rf->offset);
            const uint8_t *y = *(const uint8_t *const *)(a + f->offset);
            for (size_t k = 0; ok && k < n; k++) {
                ok = equal_value(rf, x + k * ref_elem_size(rf->type), f, y + k * elem_size(f->type));
            }
        } else if (rf->flags & PROTOBUF_C_FIELD_FLAG_ONEOF) {
            uint32_t c = *(const uint32_t *)(e + rf->quantifier_offset);
            ok = c == *(const uint32_t *)(a + f->qoffset);
            if (ok && c == rf->id) ok = equal_value(rf, e + rf->offset, f, a + f->offset);
        } else {
            ok = equal_value(rf, e + rf->offset, f, a + f->offset);
        }
        if (!ok) {
            fprintf(stderr, "%s.%s: value mismatch\n", ref->name, rf->name);
            return false;
        }
    }
    return true;
}

static size_t put_varint(uint8_t *out, uint64_t v) {
    size_t n = 0;
    for (; v >= 0x80; v >>= 7) out[n++] = (uint8_t)v | 0x80;
    out[n++] = (uint8_t)v;
    return n;
}

static size_t put_unknown_fields(uint8_t *out) {
    uint64_t tag = (uint64_t)UNKNOWN_NUMBER << 3;
    size_t n = put_varint(out, tag | 0);
    n += put_varint(out + n, 300);
    n += put_varint(out + n, tag | 2);
    out[n++] = 3;
    memset(out + n, 0xAA, 3);
    n += 3;
    n += put_varint(out + n, tag | 5);
    memset(out + n, 0xBB, 4);
    n += 4;
    n += put_varint(out + n, tag | 1);
    memset(out + n, 0xCC, 8);
    return n + 8;
}

static bool check_one(const ProtobufCMessageDescriptor *ref, const pb_msgdesc_t *desc, unsigned it) {
    s_budget = FILL_MESSAGES;
    ProtobufCMessage *m = fill(ref, 0);
    size_t n = protobuf_c_message_get_packed_size(m);
    uint8_t *wire = malloc(n + 64);
    protobuf_c_message_pack(m, wire);
    bool ok = false;

    uint8_t *owned = malloc(n ? n : 1);
    memcpy(owned, wire, n);
    void *dec = pb_unpack(desc, owned, owned, n);
    if (!dec) {
        fprintf(stderr, "#%u: pb_unpack rejected %zu bytes\n", it, n);
        goto out;
    }
    if (!pbcheck_equal(ref, m, desc, dec)) {
        fprintf(stderr, "#%u: decoded value differs\n", it);
        pb_free(dec);
        goto out;
    }
    size_t n2 = pb_packed_size(desc, dec);
    uint8_t *wire2 = malloc(n2 ? n2 : 1);
    size_t n3 = pb_pack(desc, dec, wire2);
    ok = n2 == n && n3 == n && memcmp(wire, wire2, n) == 0;
    if (!ok) fprintf(stderr, "#%u: re-encoded %zu/%zu bytes differ from %zu\n", it, n2, n3, n);
    free(wire2);
    pb_free(dec);
    if (!ok) goto out;

    size_t u = n + put_unknown_fields(wire + n);
    dec = pb_unpack(desc, NULL, wire, u);
    ok = dec && pbcheck_equal(ref, m, desc, dec);
    if (!ok) fprintf(stderr, "#%u: unknown fields not skipped\n", it);
    pb_free(dec);

out:
    if (n) {
        uint8_t *fuzz = malloc(n);
        memcpy(fuzz, wire, n);
        for (unsigned k = pick(4); k > 0; k--) fuzz[pick(n)] ^= (uint8_t)(1u << pick(8));
        size_t len = pick(2) ? n : pick(n + 1);
        dec = pb_unpack(desc, fuzz, fuzz, len);
        if (dec) {
            size_t s = pb_packed_size(desc, dec);
            uint8_t *o = malloc(s ? s : 1);
            pb_pack(desc, dec, o);
            free(o);
            pb_free(dec);
        }
    }
    protobuf_c_message_free_unpacked(m, NULL);
    free(wire);
    return ok;
}

int pbcheck_run(const ProtobufCMessageDescriptor *ref, const pb_msgdesc_t *desc, unsigned iterations) {
    int failures = 0;
    for (unsigned it = 0; it < iterations && failures < MAX_FAILURES; it++) {
        failures += !check_one(ref, desc, it);
    }
    printf("%s: %u iterations, %d failures\n", ref->name, iterations, failures);
    return failures;
}
