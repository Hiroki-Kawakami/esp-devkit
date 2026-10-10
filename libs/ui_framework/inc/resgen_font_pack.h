/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stdint.h>

typedef struct {
    uint32_t bitmap;
    uint16_t adv_w;
    uint8_t box_w;
    uint8_t box_h;
    int8_t ofs_x;
    int8_t ofs_y;
} resgen_glyph_t;

typedef struct {
    const uint16_t *codepoints;
    const resgen_glyph_t *glyphs;
    const uint8_t *data;
    uint32_t glyph_count;
    uint8_t px;
    uint8_t bpp;
    uint8_t prefilter;
    int16_t line_height;
    int16_t base_line;
    int16_t max_ascent;
    int16_t max_descent;
    int8_t underline_position;
    int8_t underline_thickness;
} resgen_font_pack_t;
