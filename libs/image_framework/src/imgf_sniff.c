/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imgf_sniff.h"

#include <string.h>

#include "imgf_bmpd.h"
#include "imgf_gifd.h"
#include "imgf_jpegd.h"
#include "imgf_pngd.h"
#include "imgf_tiffd.h"

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
    if (len >= 6 && (memcmp(h, "GIF87a", 6) == 0 || memcmp(h, "GIF89a", 6) == 0)) {
        return IMGF_FMT_GIF;
    }
    if (len >= 4 && (memcmp(h, "II*\0", 4) == 0 || memcmp(h, "MM\0*", 4) == 0)) {
        return IMGF_FMT_TIFF;
    }
    if (len >= 2 && h[0] == 'B' && h[1] == 'M') {
        return IMGF_FMT_BMP;
    }
    return IMGF_FMT_UNKNOWN;
}

imgf_decoder_t *imgf_make_decoder(imgf_format_t fmt) {
    switch (fmt) {
        case IMGF_FMT_JPEG: return imgf_jpegd_create();
        case IMGF_FMT_PNG:  return imgf_pngd_create();
        case IMGF_FMT_BMP:  return imgf_bmpd_create();
        case IMGF_FMT_GIF:  return imgf_gifd_create();
        case IMGF_FMT_TIFF: return imgf_tiffd_create();
        default:            return NULL;
    }
}
