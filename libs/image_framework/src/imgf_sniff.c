/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imgf_sniff.h"
#include "imgf_jpegd.h"
#include "imgf_pngd.h"

imgf_format_t imgf_sniff(const void *header, size_t len) {
    const uint8_t *h = (const uint8_t *)header;
    static const uint8_t kPng[8] = {0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A};
    if (len >= 8) {
        int ok = 1;
        for (int i = 0; i < 8; i++) if (h[i] != kPng[i]) { ok = 0; break; }
        if (ok) return IMGF_FMT_PNG;
    }
    if (len >= 3 && h[0] == 0xFF && h[1] == 0xD8 && h[2] == 0xFF) {
        return IMGF_FMT_JPEG;
    }
    return IMGF_FMT_UNKNOWN;
}

imgf_decoder_t *imgf_make_decoder(imgf_format_t fmt) {
    switch (fmt) {
        case IMGF_FMT_JPEG: return imgf_jpegd_create();
        case IMGF_FMT_PNG:  return imgf_pngd_create();
        default:            return NULL;
    }
}
