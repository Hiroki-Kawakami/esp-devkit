/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * jpeg_decode_enhanced — Layer 1 HOST (simulator) implementation.
 *
 * The device Layer 1 (jpeg_decode_enhanced.c) strip-pipelines the decode over
 * 2D-DMA to save PSRAM bandwidth — a hardware-only concern. On the host there
 * is no such bandwidth wall, so this backend decodes the whole frame on the CPU
 * (via image_framework's baseline JPEG decoder) into a single scratch buffer
 * and delivers it as ONE strip. That keeps the public API identical while the
 * shared Layer 2 (jpeg_ppa_pipeline.c) and app code compile and run unchanged.
 *
 * Host semantics (see README): strip_h_hint / ring_count are accepted but the
 * frame is always one strip (strip_count == 1); on_frame_start / on_strip_done
 * fire synchronously from the process() thread; release_strip / sync_strip_for_cpu
 * are no-ops. Only RGB565 / RGB888 output is supported (the panel is RGB).
 *
 * The RGB565 packing + rgb_order handling mirror idf_compat's jpeg_decode.c
 * (R-in-high-bits via BGR order) so the decoded strip is byte-for-byte what the
 * rest of the simulator — the PPA shim and the bsp framebuffer — expect.
 */

#include <stdlib.h>
#include <string.h>
#include "jpeg_decode_enhanced.h"
#include "esp_log.h"

#include "imgf_jpegd.h"
#include "imgf_decoder.h"
#include "imgf_stream.h"
#include "imgf_types.h"

static const char *TAG = "jpeg_enh_host";

struct jpeg_enh_strip_decoder_s {
    jpeg_enh_strip_decoder_cfg_t cfg;
    int out_bpp;                  // 2 (RGB565) or 3 (RGB888); 0 = unsupported format
    void *strip_buf;              // whole-frame scratch (ring mode only)
    size_t strip_buf_size;
    jpeg_enh_frame_info_t frame;  // geometry of the most recent frame
    uint32_t strips_delivered;
};

static int s_out_bpp(jpeg_dec_output_format_t f)
{
    switch (f) {
    case JPEG_DECODE_OUT_FORMAT_RGB565: return 2;
    case JPEG_DECODE_OUT_FORMAT_RGB888: return 3;
    default:                            return 0;  // GRAY / YUV: device-only
    }
}

// Pack one decoded source row (RGB888 or expanded GRAY8, src[0..2] = R,G,B) into
// the output format at dst. bgr selects the device's R-in-high-bits packing.
static void s_pack_row(const uint8_t *src, int sch, uint32_t w,
                       jpeg_dec_output_format_t fmt, bool bgr, uint8_t *dst)
{
    for (uint32_t x = 0; x < w; x++) {
        uint8_t R, G, B;
        if (sch == 1) {
            R = G = B = src[x];
        } else {
            const uint8_t *s = src + (size_t)x * sch;
            R = s[0]; G = s[1]; B = s[2];
        }
        if (fmt == JPEG_DECODE_OUT_FORMAT_RGB565) {
            uint8_t r = bgr ? R : B;
            uint8_t b = bgr ? B : R;
            ((uint16_t *)dst)[x] = (uint16_t)(((r & 0xF8) << 8) | ((G & 0xFC) << 3) | (b >> 3));
        } else {
            uint8_t *d = dst + (size_t)x * 3;
            d[0] = bgr ? B : R;
            d[1] = G;
            d[2] = bgr ? R : B;
        }
    }
}

// Decode the whole JPEG into dst (dst_capacity bytes) and fill h->frame.
static esp_err_t s_decode(jpeg_enh_strip_decoder_handle_t h,
                          const uint8_t *bit_stream, uint32_t stream_size,
                          uint8_t *dst, size_t dst_capacity)
{
    if (h->out_bpp == 0) return ESP_ERR_NOT_SUPPORTED;

    imgf_buffer_source_t bufsrc;
    imgf_stream_t stream = imgf_stream_from_buffer(&bufsrc, bit_stream, stream_size);
    imgf_decoder_t *dec = imgf_jpegd_create();
    if (!dec) return ESP_ERR_NO_MEM;

    esp_err_t err = ESP_OK;
    uint8_t *rowbuf = NULL;
    imgf_decode_opts_t opts = { 0 };  // full resolution
    imgf_err_t oe = imgf_decoder_open(dec, stream, &opts);
    if (oe != IMGF_OK) {
        err = (oe == IMGF_ERR_UNSUPPORTED) ? ESP_ERR_NOT_SUPPORTED : ESP_ERR_INVALID_STATE;
        goto done;
    }

    uint32_t w = imgf_decoder_width(dec);
    uint32_t hgt = imgf_decoder_height(dec);
    imgf_pixfmt_t spf = imgf_decoder_pixfmt(dec);
    int sch = imgf_pixfmt_bpp(spf);
    if (sch != 1 && sch != 3) { err = ESP_ERR_NOT_SUPPORTED; goto done; }

    if (w > h->cfg.max_pic_w || hgt > h->cfg.max_pic_h) {
        ESP_LOGE(TAG, "picture %ux%u exceeds configured max %ux%u",
                 (unsigned)w, (unsigned)hgt,
                 (unsigned)h->cfg.max_pic_w, (unsigned)h->cfg.max_pic_h);
        err = ESP_ERR_INVALID_SIZE;
        goto done;
    }
    size_t need = (size_t)w * hgt * h->out_bpp;
    if (need > dst_capacity) { err = ESP_ERR_INVALID_SIZE; goto done; }

    bool bgr = (h->cfg.decode.rgb_order == JPEG_DEC_RGB_ELEMENT_ORDER_BGR);
    rowbuf = malloc((size_t)w * sch);
    if (!rowbuf) { err = ESP_ERR_NO_MEM; goto done; }
    for (uint32_t y = 0; y < hgt; y++) {
        if (!imgf_decoder_next_row(dec, rowbuf)) {
            ESP_LOGE(TAG, "decode ended early at row %u/%u", (unsigned)y, (unsigned)hgt);
            err = ESP_ERR_INVALID_STATE;
            goto done;
        }
        s_pack_row(rowbuf, sch, w, h->cfg.decode.output_format, bgr,
                   dst + (size_t)y * w * h->out_bpp);
    }

    h->frame.pic_w = w;
    h->frame.pic_h = hgt;
    h->frame.origin_w = w;
    h->frame.origin_h = hgt;
    h->frame.mcu_w = 8;
    h->frame.mcu_h = 8;
    h->frame.strip_h = hgt;
    h->frame.strip_count = 1;

done:
    free(rowbuf);
    imgf_decoder_destroy(dec);
    return err;
}

esp_err_t jpeg_enh_strip_decoder_new(const jpeg_enh_strip_decoder_cfg_t *cfg,
                                     jpeg_enh_strip_decoder_handle_t *out_handle)
{
    if (!cfg || !out_handle) return ESP_ERR_INVALID_ARG;
    if (cfg->max_pic_w == 0 || cfg->max_pic_h == 0) return ESP_ERR_INVALID_ARG;
    bool strip_mode = cfg->ring_count > 0;
    if (strip_mode && !cfg->on_strip_done) return ESP_ERR_INVALID_ARG;

    int obpp = s_out_bpp(cfg->decode.output_format);
    if (obpp == 0) {
        ESP_LOGE(TAG, "host backend supports only RGB565/RGB888 output");
        return ESP_ERR_NOT_SUPPORTED;
    }

    jpeg_enh_strip_decoder_handle_t h = calloc(1, sizeof(*h));
    if (!h) return ESP_ERR_NO_MEM;
    h->cfg = *cfg;
    h->out_bpp = obpp;

    if (strip_mode) {
        h->strip_buf_size = (size_t)cfg->max_pic_w * cfg->max_pic_h * obpp;
        h->strip_buf = malloc(h->strip_buf_size);
        if (!h->strip_buf) { free(h); return ESP_ERR_NO_MEM; }
    }

    *out_handle = h;
    return ESP_OK;
}

esp_err_t jpeg_enh_strip_decoder_del(jpeg_enh_strip_decoder_handle_t h)
{
    if (!h) return ESP_OK;
    free(h->strip_buf);
    free(h);
    return ESP_OK;
}

esp_err_t jpeg_enh_strip_decoder_process(jpeg_enh_strip_decoder_handle_t h,
                                         const uint8_t *bit_stream, uint32_t stream_size,
                                         jpeg_enh_frame_info_t *info)
{
    if (!h || !bit_stream || !stream_size) return ESP_ERR_INVALID_ARG;
    if (!h->strip_buf) return ESP_ERR_INVALID_STATE;  // ring_count = 0 handle

    h->strips_delivered = 0;
    esp_err_t err = s_decode(h, bit_stream, stream_size, h->strip_buf, h->strip_buf_size);
    if (err != ESP_OK) return err;

    if (info) *info = h->frame;
    if (h->cfg.on_frame_start) {
        err = h->cfg.on_frame_start(&h->frame, h->cfg.user_ctx);
        if (err != ESP_OK) return err;
    }

    if (h->cfg.on_strip_done) {
        jpeg_enh_strip_event_t e = {
            .strip_idx   = 0,
            .buffer      = h->strip_buf,
            .y_offset    = 0,
            .rows        = h->frame.origin_h,
            .padded_rows = h->frame.pic_h,
        };
        h->cfg.on_strip_done(&e, h->cfg.user_ctx);
    }
    h->strips_delivered = 1;
    return ESP_OK;
}

esp_err_t jpeg_enh_decoder_process(jpeg_enh_strip_decoder_handle_t h,
                                   const uint8_t *bit_stream, uint32_t stream_size,
                                   void *out_buf, size_t out_buf_size,
                                   jpeg_enh_frame_info_t *info)
{
    if (!h || !bit_stream || !stream_size || !out_buf) return ESP_ERR_INVALID_ARG;

    esp_err_t err = s_decode(h, bit_stream, stream_size, out_buf, out_buf_size);
    if (err != ESP_OK) return err;

    if (info) *info = h->frame;
    if (h->cfg.on_frame_start) {
        err = h->cfg.on_frame_start(&h->frame, h->cfg.user_ctx);
        if (err != ESP_OK) return err;
    }
    return ESP_OK;
}

esp_err_t jpeg_enh_strip_decoder_release_strip(jpeg_enh_strip_decoder_handle_t h, uint32_t strip_idx)
{
    (void)strip_idx;
    if (!h || !h->strip_buf) return ESP_ERR_INVALID_ARG;
    return ESP_OK;  // no backpressure on the host (whole-frame decode)
}

esp_err_t jpeg_enh_strip_decoder_sync_strip_for_cpu(jpeg_enh_strip_decoder_handle_t h,
                                                    const jpeg_enh_strip_event_t *evt)
{
    if (!h || !evt || !evt->buffer) return ESP_ERR_INVALID_ARG;
    return ESP_OK;  // host memory is coherent
}

uint32_t jpeg_enh_strip_decoder_strips_delivered(jpeg_enh_strip_decoder_handle_t h)
{
    return h ? h->strips_delivered : 0;
}

size_t jpeg_enh_strip_decoder_strip_buffer_size(jpeg_enh_strip_decoder_handle_t h)
{
    return h ? h->strip_buf_size : 0;
}
