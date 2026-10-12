/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct {
    size_t len;
    uint8_t *data;
} pb_bytes_t;

enum {
    PB_TYPE_INT32,
    PB_TYPE_INT64,
    PB_TYPE_UINT32,
    PB_TYPE_UINT64,
    PB_TYPE_BOOL,
    PB_TYPE_BYTES,
    PB_TYPE_MESSAGE,
};

#define PB_FLAG_REPEATED 0x01
#define PB_FLAG_ONEOF    0x02

typedef struct pb_msgdesc pb_msgdesc_t;

typedef struct {
    uint16_t number;
    uint8_t type;
    uint8_t flags;
    uint16_t offset;
    // n_<field> of a repeated field, <oneof>_case of a oneof member.
    uint16_t qoffset;
    const pb_msgdesc_t *msg;
} pb_field_t;

struct pb_msgdesc {
    uint16_t size;
    uint16_t n_fields;
    const pb_field_t *fields;
};

size_t pb_packed_size(const pb_msgdesc_t *desc, const void *msg);
size_t pb_pack(const pb_msgdesc_t *desc, const void *msg, uint8_t *out);

// Bytes fields point into `data`. `owner` (heap or NULL) is freed by pb_free,
// or before returning NULL on malformed input or allocation failure.
void *pb_unpack(const pb_msgdesc_t *desc, void *owner, const uint8_t *data, size_t len);
void pb_free(void *msg);
