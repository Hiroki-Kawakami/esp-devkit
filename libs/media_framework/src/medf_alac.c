/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "medf_alac.h"

#include <string.h>

#define ATOM_HEADER_BYTES 12
#define FULL_BOX_BYTES 4

static uint32_t be32(const uint8_t *p) {
    return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
}

const uint8_t *medf_alac_find_cookie(const uint8_t *data, size_t size) {
    size_t pos = 0;
    if (size - pos >= ATOM_HEADER_BYTES && memcmp(data + pos + 4, "frma", 4) == 0) pos += ATOM_HEADER_BYTES;
    if (size - pos >= ATOM_HEADER_BYTES && memcmp(data + pos + 4, "alac", 4) == 0) {
        pos += ATOM_HEADER_BYTES;
    } else if (size - pos >= FULL_BOX_BYTES + MEDF_ALAC_COOKIE_BYTES && be32(data + pos) == 0) {
        pos += FULL_BOX_BYTES;
    }
    if (size - pos < MEDF_ALAC_COOKIE_BYTES || be32(data + pos) == 0) return NULL;
    return data + pos;
}

void medf_alac_cookie_format(const uint8_t *cookie, uint32_t *sample_rate, uint8_t *channels,
                             uint8_t *bits_per_sample) {
    *sample_rate = be32(cookie + 20);
    *channels = cookie[9];
    *bits_per_sample = cookie[5];
}
