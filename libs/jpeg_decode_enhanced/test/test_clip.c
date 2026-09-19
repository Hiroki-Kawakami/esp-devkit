/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "jpeg_ppa_pipeline.h"

#define STRIP_H 16
#define BPP 3
#define SENTINEL 0xA5

struct jpeg_enh_strip_decoder_s {
    jpeg_enh_strip_decoder_cfg_t cfg;
    uint32_t delivered;
};

static const uint8_t *s_src;
static uint32_t s_src_w, s_src_h;

esp_err_t jpeg_enh_strip_decoder_new(const jpeg_enh_strip_decoder_cfg_t *cfg,
                                     jpeg_enh_strip_decoder_handle_t *out_handle)
{
    jpeg_enh_strip_decoder_handle_t h = calloc(1, sizeof(*h));
    if (!h) return ESP_ERR_NO_MEM;
    h->cfg = *cfg;
    *out_handle = h;
    return ESP_OK;
}

esp_err_t jpeg_enh_strip_decoder_del(jpeg_enh_strip_decoder_handle_t handle)
{
    free(handle);
    return ESP_OK;
}

esp_err_t jpeg_enh_strip_decoder_process(jpeg_enh_strip_decoder_handle_t h,
                                         const uint8_t *bit_stream, uint32_t stream_size,
                                         jpeg_enh_frame_info_t *out_info)
{
    (void)bit_stream;
    (void)stream_size;
    jpeg_enh_frame_info_t info = {
        .pic_w = s_src_w,
        .pic_h = (s_src_h + STRIP_H - 1) / STRIP_H * STRIP_H,
        .origin_w = s_src_w,
        .origin_h = s_src_h,
        .mcu_w = 16,
        .mcu_h = 16,
        .strip_h = STRIP_H,
        .strip_count = (s_src_h + STRIP_H - 1) / STRIP_H,
    };
    if (out_info) *out_info = info;
    h->delivered = 0;
    if (h->cfg.on_frame_start) {
        esp_err_t err = h->cfg.on_frame_start(&info, h->cfg.user_ctx);
        if (err != ESP_OK) return err;
    }
    for (uint32_t i = 0; i < info.strip_count; i++) {
        uint8_t *buf = h->cfg.strip_bufs[i % 2];
        uint32_t y = i * STRIP_H;
        uint32_t rows = s_src_h - y < STRIP_H ? s_src_h - y : STRIP_H;
        memset(buf, 0, (size_t)s_src_w * STRIP_H * BPP);
        memcpy(buf, s_src + (size_t)y * s_src_w * BPP, (size_t)rows * s_src_w * BPP);
        jpeg_enh_strip_event_t evt = {
            .strip_idx = i, .buffer = buf, .y_offset = y, .rows = rows, .padded_rows = STRIP_H,
        };
        h->delivered++;
        h->cfg.on_strip_done(&evt, h->cfg.user_ctx);
    }
    return ESP_OK;
}

esp_err_t jpeg_enh_strip_decoder_release_strip(jpeg_enh_strip_decoder_handle_t handle, uint32_t strip_idx)
{
    (void)handle;
    (void)strip_idx;
    return ESP_OK;
}

uint32_t jpeg_enh_strip_decoder_strips_delivered(jpeg_enh_strip_decoder_handle_t handle)
{
    return handle->delivered;
}

typedef struct { int x, y, w, h; } rect_t;

static int s_failures;

static void make_source(uint8_t *src, uint32_t w, uint32_t h)
{
    for (uint32_t y = 0; y < h; y++) {
        for (uint32_t x = 0; x < w; x++) {
            uint8_t *p = src + ((size_t)y * w + x) * BPP;
            p[0] = (uint8_t)(x * 127 / (w - 1));
            p[1] = (uint8_t)(y * 127 / (h - 1));
            p[2] = (uint8_t)((x + y) * 127 / (w + h - 2));
        }
    }
}

static esp_err_t render(jpeg_ppa_pipeline_handle_t pipe, uint8_t *out, uint32_t out_w, uint32_t out_h,
                        const jpeg_ppa_transform_t *t)
{
    memset(out, SENTINEL, (size_t)out_w * out_h * BPP);
    jpeg_ppa_output_t o = {
        .buffer = out, .pic_w = out_w, .pic_h = out_h, .color_mode = PPA_SRM_COLOR_MODE_RGB888,
    };
    static const uint8_t dummy[4];
    return jpeg_ppa_pipeline_process(pipe, dummy, sizeof(dummy), &o, t, NULL);
}

static uint32_t fnv(const uint8_t *p, size_t n)
{
    uint32_t h = 2166136261u;
    for (size_t i = 0; i < n; i++) h = (h ^ p[i]) * 16777619u;
    return h;
}

static bool is_sentinel(const uint8_t *p)
{
    return p[0] == SENTINEL && p[1] == SENTINEL && p[2] == SENTINEL;
}

static bool inside(rect_t r, int x, int y)
{
    return x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h;
}

static rect_t intersect(rect_t a, rect_t b)
{
    int x0 = a.x > b.x ? a.x : b.x, y0 = a.y > b.y ? a.y : b.y;
    int x1 = a.x + a.w < b.x + b.w ? a.x + a.w : b.x + b.w;
    int y1 = a.y + a.h < b.y + b.h ? a.y + a.h : b.y + b.h;
    rect_t r = { x0, y0, x1 - x0, y1 - y0 };
    if (r.w <= 0 || r.h <= 0) r = (rect_t){ 0, 0, 0, 0 };
    return r;
}

static void check_clip(const char *name, const uint8_t *full, const uint8_t *clipped,
                       uint32_t out_w, uint32_t out_h, rect_t rendered, rect_t clip, int gap)
{
    rect_t keep = intersect(rendered, clip);
    rect_t core = { keep.x + gap, keep.y + gap, keep.w - 2 * gap, keep.h - 2 * gap };
    int outside_written = 0, core_missing = 0, max_diff = 0;
    for (int y = 0; y < (int)out_h; y++) {
        for (int x = 0; x < (int)out_w; x++) {
            const uint8_t *c = clipped + ((size_t)y * out_w + x) * BPP;
            const uint8_t *f = full + ((size_t)y * out_w + x) * BPP;
            if (!inside(keep, x, y)) {
                if (!is_sentinel(c)) outside_written++;
                continue;
            }
            if (!inside(core, x, y)) continue;
            if (is_sentinel(c)) {
                if (!is_sentinel(f)) core_missing++;
                continue;
            }
            for (int i = 0; i < BPP; i++) {
                int d = abs((int)c[i] - (int)f[i]);
                if (d > max_diff) max_diff = d;
            }
        }
    }
    if (outside_written || core_missing || max_diff > 6) {
        printf("FAIL %s: written outside clip %d, unwritten inside %d, max diff %d\n",
               name, outside_written, core_missing, max_diff);
        s_failures++;
    }
}

int main(int argc, char **argv)
{
    const bool dump = argc > 1 && strcmp(argv[1], "--dump") == 0;
    const uint32_t out_w = 400, out_h = 360;
    uint8_t *out_full = malloc((size_t)out_w * out_h * BPP);
    uint8_t *out_clip = malloc((size_t)out_w * out_h * BPP);
    static const struct { uint32_t w, h; } sizes[] = { { 160, 120 }, { 96, 72 }, { 200, 100 } };
    static const uint32_t scales[] = { 8, 16, 18, 20, 24, 27, 32 };
    int cases = 0;

    for (size_t si = 0; si < sizeof(sizes) / sizeof(sizes[0]); si++) {
        s_src_w = sizes[si].w;
        s_src_h = sizes[si].h;
        uint8_t *src = malloc((size_t)s_src_w * s_src_h * BPP);
        make_source(src, s_src_w, s_src_h);
        s_src = src;
        size_t strip_bytes = (size_t)s_src_w * STRIP_H * BPP;
        void *strips[2] = { malloc(strip_bytes), malloc(strip_bytes) };
        jpeg_ppa_pipeline_cfg_t cfg = {
            .strip_bufs = { strips[0], strips[1] },
            .strip_buf_size = strip_bytes,
            .strip_color_mode = PPA_SRM_COLOR_MODE_RGB888,
        };
        jpeg_ppa_pipeline_handle_t pipe;
        if (jpeg_ppa_pipeline_new(&cfg, &pipe) != ESP_OK) return 1;

        for (size_t ni = 0; ni < sizeof(scales) / sizeof(scales[0]); ni++) {
            uint32_t n = scales[ni];
            for (int crop = 0; crop < 2; crop++) {
                jpeg_ppa_rect_t in_crop = crop ? (jpeg_ppa_rect_t){ 6, 16, s_src_w - 20, s_src_h - 40 }
                                               : (jpeg_ppa_rect_t){ 0 };
                uint32_t cw = crop ? in_crop.w : s_src_w, chh = crop ? in_crop.h : s_src_h;
                for (int rot = 0; rot < 4; rot++) {
                    uint32_t sw = cw * n / 16, sh = chh * n / 16;
                    uint32_t ew = rot & 1 ? sh : sw, eh = rot & 1 ? sw : sh;
                    if (ew + 4 > out_w || eh + 4 > out_h) continue;
                    for (int mirror = 0; mirror < 4; mirror++) {
                        jpeg_ppa_transform_t t = {
                            .rotation = (ppa_srm_rotation_angle_t)rot,
                            .scale_x = n / 16.0f,
                            .scale_y = n / 16.0f,
                            .mirror_x = mirror & 1,
                            .mirror_y = (mirror >> 1) & 1,
                            .in_crop = in_crop,
                            .out_offset_x = (out_w - ew) / 2,
                            .out_offset_y = (out_h - eh) / 2,
                        };
                        char name[128];
                        snprintf(name, sizeof(name), "src %ux%u n %u crop %d rot %d mirror %d",
                                 (unsigned)s_src_w, (unsigned)s_src_h, (unsigned)n, crop, rot, mirror);
                        if (render(pipe, out_full, out_w, out_h, &t) != ESP_OK) {
                            printf("FAIL %s: unclipped render\n", name);
                            s_failures++;
                            continue;
                        }
                        cases++;
                        if (dump) {
                            printf("%s %08x\n", name, (unsigned)fnv(out_full, (size_t)out_w * out_h * BPP));
                            continue;
                        }
#ifdef HAVE_OUT_CLIP
                        rect_t rendered = { (int)t.out_offset_x, (int)t.out_offset_y, (int)ew, (int)eh };
                        const int b = 13;
                        const rect_t clips[] = {
                            { 0, 0, (int)out_w, rendered.y + rendered.h - b },
                            { 0, rendered.y + b, (int)out_w, (int)out_h },
                            { 0, 0, rendered.x + rendered.w - b, (int)out_h },
                            { rendered.x + b, 0, (int)out_w, (int)out_h },
                            { 0, rendered.y + b, (int)out_w, rendered.h - 2 * b - 3 },
                            { rendered.x + b + 1, 0, rendered.w - 2 * b - 1, (int)out_h },
                            { rendered.x + b, rendered.y + 7, rendered.w - 2 * b, rendered.h - 17 },
                            { 0, 0, (int)out_w, rendered.y + 1 },
                            { 0, 0, (int)out_w, rendered.y },
                        };
                        int gap = (int)((n + 15) / 16) + 1;
                        for (size_t ci = 0; ci < sizeof(clips) / sizeof(clips[0]); ci++) {
                            rect_t k = clips[ci];
                            if (k.w <= 0 || k.h <= 0) continue;
                            t.out_clip = (jpeg_ppa_rect_t){ (uint32_t)k.x, (uint32_t)k.y, (uint32_t)k.w, (uint32_t)k.h };
                            char cname[160];
                            snprintf(cname, sizeof(cname), "%s clip %zu", name, ci);
                            if (render(pipe, out_clip, out_w, out_h, &t) != ESP_OK) {
                                printf("FAIL %s: clipped render\n", cname);
                                s_failures++;
                                continue;
                            }
                            check_clip(cname, out_full, out_clip, out_w, out_h, rendered, k, gap);
                            cases++;
                        }
#endif
                    }
                }
            }
        }
        jpeg_ppa_pipeline_del(pipe);
        free(strips[0]);
        free(strips[1]);
        free(src);
    }
    free(out_full);
    free(out_clip);
    if (!dump) printf("%d cases, %d failures\n", cases, s_failures);
    return s_failures ? 1 : 0;
}
