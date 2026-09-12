/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Panel capture without a frame buffer: an imgf_stream_t that pulls one panel
 * row at a time through bsp_harness_display_read feeds the JPEG encoder, whose
 * output goes straight to a sink. Peak memory is one row plus the encoder's
 * MCU-row working set, so it fits boards without PSRAM.
 */

#include "harness_internal.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include "bsp.h"
#include "bsp_harness.h"
#include "esp_heap_caps.h"
#include "imgf_encoder.h"
#include "imgf_jpege.h"

typedef struct {
    int                y;
    bsp_size_t         size;
    bsp_pixel_format_t fmt;
    size_t             row_bytes;
    esp_err_t          err;
} row_source_t;

/* LVGL keeps RGB888 as B,G,R in memory; the encoder wants R,G,B. */
static void swap_rb(uint8_t *row, int w) {
    for (int x = 0; x < w; x++, row += 3) {
        uint8_t t = row[0]; row[0] = row[2]; row[2] = t;
    }
}

static int row_source_read(void *user, void *dst, size_t n) {
    row_source_t *s = user;
    if (n != s->row_bytes || s->y >= s->size.height) return -1;
    const bsp_rect_t area = { { 0, s->y }, { s->size.width, 1 } };
    s->err = bsp_harness_display_read(area, dst);
    if (s->err != ESP_OK) return -1;
    if (s->fmt == BSP_PIXEL_FORMAT_RGB888) swap_rb(dst, s->size.width);
    s->y++;
    return (int)n;
}

static imgf_pixfmt_t to_imgf(bsp_pixel_format_t f) {
    switch (f) {
        case BSP_PIXEL_FORMAT_L8:     return IMGF_PIX_GRAY8;
        case BSP_PIXEL_FORMAT_RGB888: return IMGF_PIX_RGB888;
        default:                      return IMGF_PIX_RGB565;
    }
}

esp_err_t harness_capture(imgf_sink_t sink, int quality) {
    if (!(bsp_display_get_caps() & BSP_DISPLAY_CAP_READBACK)) return ESP_ERR_NOT_SUPPORTED;

    row_source_t src = {
        .size = bsp_display_get_size(),
        .fmt  = bsp_display_get_pixel_format(),
    };
    src.row_bytes = (size_t)src.size.width * bsp_pixel_format_bytes(src.fmt);
    uint8_t *row = malloc(src.row_bytes);
    if (!row) return ESP_ERR_NO_MEM;

    imgf_jpege_opts_t opts = { .quality = quality, .alloc_caps = MALLOC_CAP_8BIT };
    imgf_err_t ierr;
    imgf_encoder_t *enc = imgf_jpege_create((uint16_t)src.size.width, (uint16_t)src.size.height,
                                            to_imgf(src.fmt), &opts, &ierr);
    if (!enc) { free(row); return ierr == IMGF_ERR_OOM ? ESP_ERR_NO_MEM : ESP_FAIL; }

    esp_err_t err = ESP_OK;
    harness_lock();
    ierr = imgf_encoder_bind_sink(enc, sink);
    if (ierr == IMGF_OK) {
        imgf_stream_t stream = { row_source_read, &src };
        ierr = imgf_encoder_encode_stream(enc, stream, row, NULL);
    }
    harness_unlock();
    if (src.err != ESP_OK)      err = src.err;
    else if (ierr != IMGF_OK)   err = ierr == IMGF_ERR_OOM ? ESP_ERR_NO_MEM : ESP_FAIL;

    imgf_encoder_destroy(enc);
    free(row);
    return err;
}

static void make_parent_dirs(const char *path) {
    char buf[512];
    size_t n = strlen(path);
    if (n == 0 || n >= sizeof(buf)) return;
    memcpy(buf, path, n + 1);
    for (char *p = buf + 1; *p; p++) {
        if (*p != '/') continue;
        *p = '\0';
        mkdir(buf, 0755);
        *p = '/';
    }
}

static int file_sink_write(void *user, const void *data, size_t n) {
    return (int)fwrite(data, 1, n, (FILE *)user);
}

bool harness_capture_to_file(const char *path, int quality) {
    make_parent_dirs(path);
    FILE *f = fopen(path, "wb");
    if (!f) return false;
    imgf_sink_t sink = { file_sink_write, f };
    esp_err_t err = harness_capture(sink, quality);
    fclose(f);
    if (err != ESP_OK) remove(path);
    return err == ESP_OK;
}
