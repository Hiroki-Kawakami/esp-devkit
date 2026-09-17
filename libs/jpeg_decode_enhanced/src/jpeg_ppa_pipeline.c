/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * jpeg_decode_enhanced — Layer 2 implementation.
 *
 * Each jpeg_ppa_pipeline_process() call pushes a single JPEG frame through:
 *   1) strip-decode into the strip buffers (Layer 1, in the calling thread)
 *   2) per-strip PPA SRM into the destination frame buffer
 *
 * Strips are pumped entirely from ISR context: the strip-done callback
 * submits the strip to PPA when it is idle (otherwise parks it), and the PPA
 * done callback releases the finished strip and submits the next parked one.
 * The decoder stalls while every strip buffer is unreleased, so at most
 * MAX_PARKED strips are ever parked.
 *
 * Wait-for-frame: the decode call returns once the JPEG hardware reports
 * RX_EOF, but the last PPA strips may still be in flight; process() blocks on
 * all_done (given after the final strip) before returning.
 *
 * Strip placement: the per-frame transform is resolved in the on_frame_start
 * callback (decode-thread context, before any strip arrives) into integer
 * "sixteenths" scale factors matching PPA's 1/16 quantization. Each strip's
 * output rect is then derived by mapping its input row band through
 * scale -> rotate -> mirror over the cropped image extent, so all rotations,
 * mirrors and crops share one placement computation.
 */

#include <string.h>
#include "jpeg_ppa_pipeline.h"
#include "ppa_srm_fast.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#ifdef ESP_PLATFORM
#include "esp_attr.h"
#else
#define IRAM_ATTR
#endif

static const char *TAG = "jpeg_ppa";

#define DEFAULT_TIMEOUT_MS    200
#define SCALE_FRAG_MAX        16        // PPA scale quantization: 1/16 steps
#define MAX_PARKED            2

struct jpeg_ppa_pipeline_s {
    jpeg_ppa_pipeline_cfg_t cfg;
    jpeg_enh_strip_decoder_handle_t decoder;
    ppa_srm_fast_handle_t srm;
    SemaphoreHandle_t all_done;
    TickType_t timeout;
    bool broken;                         // an SRM submission could not be cancelled
    bool in_process;                     // on_frame_start belongs to process(), not a whole-frame decode

    // Per-frame state, written by process()/on_frame_start before any strip
    // arrives, read-only afterwards.
    struct {
        jpeg_enh_frame_info_t frame;
        jpeg_ppa_output_t out;           // buffer_size resolved
        jpeg_ppa_transform_t t;          // scales normalized (never 0)
        jpeg_ppa_rect_t crop;            // resolved input crop (within origin)
        uint32_t fx, fy;                 // quantized scales in 1/16 units
        uint32_t scaled_w;               // crop.w scaled by fx (output px)
        uint32_t scaled_h;               // crop.h scaled by fy (output px)
        uint32_t ext_w, ext_h;           // scaled crop extent after rotation
        bool valid;                      // on_frame_start completed (SRM engine held)
    } cur;

    // Strip pump, guarded by lock
    portMUX_TYPE lock;
    jpeg_enh_strip_event_t parked[MAX_PARKED];
    uint32_t parked_head, parked_count;
    bool busy;                           // a strip is in PPA
    uint32_t busy_strip;
    bool abort_frame;

    volatile uint32_t pending_strips;
    volatile esp_err_t frame_err;
};

static inline uint32_t s_scaled(uint32_t v, uint32_t f16) { return (uint32_t)(((uint64_t)v * f16) / SCALE_FRAG_MAX); }

static uint32_t s_quantize_scale(float s, bool yuv420_out)
{
    uint32_t i = (uint32_t)s;
    uint32_t frag = (uint32_t)(s * SCALE_FRAG_MAX) & (SCALE_FRAG_MAX - 1);
    if (yuv420_out) frag &= ~1u;  // PPA forces even frag for YUV420 output
    return i * SCALE_FRAG_MAX + frag;
}

static jpeg_dec_output_format_t s_jpeg_out_for_strip_cm(ppa_srm_color_mode_t cm)
{
    switch (cm) {
    case PPA_SRM_COLOR_MODE_RGB565: return JPEG_DECODE_OUT_FORMAT_RGB565;
    case PPA_SRM_COLOR_MODE_RGB888: return JPEG_DECODE_OUT_FORMAT_RGB888;
    case PPA_SRM_COLOR_MODE_YUV420: return JPEG_DECODE_OUT_FORMAT_YUV420;
    case PPA_SRM_COLOR_MODE_YUV444: return JPEG_DECODE_OUT_FORMAT_YUV444;
    default:                        return (jpeg_dec_output_format_t)-1;
    }
}

static inline bool s_cm_is_yuv(ppa_srm_color_mode_t cm)
{
    return cm == PPA_SRM_COLOR_MODE_YUV420 || cm == PPA_SRM_COLOR_MODE_YUV444;
}

// Bits per pixel of a PPA SRM color mode. Kept local so this file has no HAL
// dependency and compiles unchanged for both the device and the host simulator.
static inline uint32_t s_cm_bits(ppa_srm_color_mode_t cm)
{
    switch (cm) {
    case PPA_SRM_COLOR_MODE_ARGB8888: return 32;
    case PPA_SRM_COLOR_MODE_RGB888:   return 24;
    case PPA_SRM_COLOR_MODE_RGB565:   return 16;
    case PPA_SRM_COLOR_MODE_YUV444:   return 24;
    case PPA_SRM_COLOR_MODE_YUV420:   return 12;
    default:                          return 0;
    }
}

// -----------------------------------------------------------------------------
// Per-frame transform resolution (decode-thread context, before strips flow)
// -----------------------------------------------------------------------------

static esp_err_t s_on_frame_start(const jpeg_enh_frame_info_t *info, void *user_ctx)
{
    jpeg_ppa_pipeline_handle_t h = (jpeg_ppa_pipeline_handle_t)user_ctx;
    if (!h->in_process) return ESP_OK;
    h->cur.frame = *info;

    // Resolve the input crop against the valid (non-padded) image area.
    jpeg_ppa_rect_t c = h->cur.t.in_crop;
    if (c.w == 0 || c.h == 0) {
        c = (jpeg_ppa_rect_t){ 0, 0, info->origin_w, info->origin_h };
    }
    if (c.x >= info->origin_w || c.y >= info->origin_h) {
        ESP_LOGE(TAG, "in_crop origin (%lu,%lu) outside image %lux%lu",
                 (unsigned long)c.x, (unsigned long)c.y,
                 (unsigned long)info->origin_w, (unsigned long)info->origin_h);
        return ESP_ERR_INVALID_ARG;
    }
    if (c.x + c.w > info->origin_w) c.w = info->origin_w - c.x;
    if (c.y + c.h > info->origin_h) c.h = info->origin_h - c.y;
    h->cur.crop = c;

    bool yuv420_out = (h->cur.out.color_mode == PPA_SRM_COLOR_MODE_YUV420);
    h->cur.fx = s_quantize_scale(h->cur.t.scale_x, yuv420_out);
    h->cur.fy = s_quantize_scale(h->cur.t.scale_y, yuv420_out);
    if (h->cur.fx == 0 || h->cur.fy == 0) {
        ESP_LOGE(TAG, "scale below PPA minimum (1/16)");
        return ESP_ERR_INVALID_ARG;
    }

    // Strips tile the output along the scaled y axis, so every interior strip
    // boundary must land on a whole output row once scaled.
    for (uint32_t k = c.y / info->strip_h + 1; (uint64_t)k * info->strip_h < c.y + c.h; k++) {
        uint32_t rel = k * info->strip_h - c.y;
        if ((rel * h->cur.fy) % SCALE_FRAG_MAX != 0) {
            ESP_LOGE(TAG, "strip boundary %lu x scale %lu/16 is not a whole output row; "
                          "adjust scale_y or in_crop.y",
                     (unsigned long)rel, (unsigned long)h->cur.fy);
            return ESP_ERR_INVALID_ARG;
        }
    }

    h->cur.scaled_w = s_scaled(c.w, h->cur.fx);
    h->cur.scaled_h = s_scaled(c.h, h->cur.fy);
    bool swap = (h->cur.t.rotation == PPA_SRM_ROTATION_ANGLE_90 ||
                 h->cur.t.rotation == PPA_SRM_ROTATION_ANGLE_270);
    h->cur.ext_w = swap ? h->cur.scaled_h : h->cur.scaled_w;
    h->cur.ext_h = swap ? h->cur.scaled_w : h->cur.scaled_h;

    if (h->cur.t.out_offset_x + h->cur.ext_w > h->cur.out.pic_w ||
        h->cur.t.out_offset_y + h->cur.ext_h > h->cur.out.pic_h) {
        ESP_LOGE(TAG, "rendered rect %lux%lu at (%lu,%lu) exceeds output %lux%lu",
                 (unsigned long)h->cur.ext_w, (unsigned long)h->cur.ext_h,
                 (unsigned long)h->cur.t.out_offset_x, (unsigned long)h->cur.t.out_offset_y,
                 (unsigned long)h->cur.out.pic_w, (unsigned long)h->cur.out.pic_h);
        return ESP_ERR_INVALID_ARG;
    }

    ppa_srm_oper_config_t op = {
        .in = {
            .pic_w          = info->pic_w,
            .block_offset_x = c.x,
            .block_w        = c.w,
            .srm_cm         = h->cfg.strip_color_mode,
        },
        .out = {
            .buffer      = h->cur.out.buffer,
            .buffer_size = h->cur.out.buffer_size,
            .pic_w       = h->cur.out.pic_w,
            .pic_h       = h->cur.out.pic_h,
            .srm_cm      = h->cur.out.color_mode,
            .yuv_range   = h->cur.out.yuv_range,
            .yuv_std     = h->cur.out.yuv_std,
        },
        .rotation_angle = h->cur.t.rotation,
        .scale_x        = h->cur.t.scale_x,
        .scale_y        = h->cur.t.scale_y,
        .mirror_x       = h->cur.t.mirror_x,
        .mirror_y       = h->cur.t.mirror_y,
        .rgb_swap       = h->cur.t.rgb_swap,
        .byte_swap      = h->cur.t.byte_swap,
    };
    if (s_cm_is_yuv(h->cfg.strip_color_mode)) {
        op.in.yuv_range = h->cfg.yuv_full_range ? PPA_COLOR_RANGE_FULL : PPA_COLOR_RANGE_LIMIT;
        op.in.yuv_std   = (h->cfg.conv_std == JPEG_YUV_RGB_CONV_STD_BT709)
                          ? PPA_COLOR_CONV_STD_RGB_YUV_BT709 : PPA_COLOR_CONV_STD_RGB_YUV_BT601;
    }
    esp_err_t err = ppa_srm_fast_begin_frame(h->srm, &op);
    if (err != ESP_OK) return err;

    h->cur.valid = true;
    h->pending_strips = info->strip_count;
    return ESP_OK;
}

// -----------------------------------------------------------------------------
// Strip pump (ISR context on device, process() thread on host)
//
// Everything reachable from here stays in IRAM: flash code run from the
// 2D-DMA ISR occasionally faulted with "Illegal instruction" on the first
// fetch of a new cache line (ESP32-P4 rev1.0, IDF v6.1).
// -----------------------------------------------------------------------------

// Map strip rows [a_rel, b_rel) (crop-relative) through scale+rotation+mirror
// into the output rect for this strip's PPA op.
static IRAM_ATTR void s_strip_out_rect(jpeg_ppa_pipeline_handle_t h, uint32_t a_rel, uint32_t b_rel,
                                       uint32_t *out_x, uint32_t *out_y)
{
    uint32_t A = s_scaled(a_rel, h->cur.fy);
    uint32_t B = s_scaled(b_rel, h->cur.fy);
    uint32_t band = B - A;
    uint32_t x, y, w, hgt;

    // Placement convention matches PPA hardware behaviour as observed on
    // ESP32-P4 (strip 0 = image top): ANGLE_90 puts the top input band at
    // output x=0, ANGLE_270 at the right edge.
    switch (h->cur.t.rotation) {
    default:
    case PPA_SRM_ROTATION_ANGLE_0:
        x = 0;                       y = A;                       w = h->cur.scaled_w; hgt = band;
        break;
    case PPA_SRM_ROTATION_ANGLE_90:
        x = A;                       y = 0;                       w = band;            hgt = h->cur.scaled_w;
        break;
    case PPA_SRM_ROTATION_ANGLE_180:
        x = 0;                       y = h->cur.scaled_h - B;     w = h->cur.scaled_w; hgt = band;
        break;
    case PPA_SRM_ROTATION_ANGLE_270:
        x = h->cur.scaled_h - B;     y = 0;                       w = band;            hgt = h->cur.scaled_w;
        break;
    }

    // PPA mirrors the pixels inside each block; flipping the block placement
    // across the full extent completes the global mirror.
    if (h->cur.t.mirror_x) x = h->cur.ext_w - x - w;
    if (h->cur.t.mirror_y) y = h->cur.ext_h - y - hgt;

    *out_x = h->cur.t.out_offset_x + x;
    *out_y = h->cur.t.out_offset_y + y;
}

static IRAM_ATTR void s_set_frame_err(jpeg_ppa_pipeline_handle_t h, esp_err_t err)
{
    if (h->frame_err == ESP_OK) h->frame_err = err;
}

// Submit the strip's crop band to PPA. ESP_ERR_NOT_FOUND = nothing to render.
static IRAM_ATTR esp_err_t s_submit_strip(jpeg_ppa_pipeline_handle_t h, const jpeg_enh_strip_event_t *evt)
{
    const jpeg_ppa_rect_t *c = &h->cur.crop;
    uint32_t a_abs = c->y > evt->y_offset ? c->y : evt->y_offset;
    uint32_t strip_end = evt->y_offset + evt->rows;
    uint32_t crop_end = c->y + c->h;
    uint32_t b_abs = strip_end < crop_end ? strip_end : crop_end;
    if (b_abs <= a_abs) return ESP_ERR_NOT_FOUND;

    uint32_t out_x, out_y;
    s_strip_out_rect(h, a_abs - c->y, b_abs - c->y, &out_x, &out_y);
    return ppa_srm_fast_submit(h->srm, evt->buffer, evt->padded_rows,
                               a_abs - evt->y_offset, b_abs - a_abs, out_x, out_y);
}

static IRAM_ATTR bool s_finish_strip(jpeg_ppa_pipeline_handle_t h, uint32_t strip_idx)
{
    jpeg_enh_strip_decoder_release_strip(h->decoder, strip_idx);
    if (__atomic_sub_fetch(&h->pending_strips, 1, __ATOMIC_ACQ_REL) != 0) return false;
    BaseType_t hp = pdFALSE;
    xSemaphoreGiveFromISR(h->all_done, &hp);
    return hp == pdTRUE;
}

// Called with busy already claimed for evt. Keeps submitting until a strip
// is in flight or nothing is parked.
static IRAM_ATTR bool s_pump(jpeg_ppa_pipeline_handle_t h, jpeg_enh_strip_event_t evt)
{
    bool yield = false;
    while (true) {
        h->busy_strip = evt.strip_idx;
        esp_err_t err = s_submit_strip(h, &evt);
        if (err == ESP_OK) return yield;
        if (err != ESP_ERR_NOT_FOUND) s_set_frame_err(h, err);
        yield |= s_finish_strip(h, evt.strip_idx);

        portENTER_CRITICAL_ISR(&h->lock);
        bool more = !h->abort_frame && h->parked_count > 0;
        if (more) {
            evt = h->parked[h->parked_head];
            h->parked_head = (h->parked_head + 1) % MAX_PARKED;
            h->parked_count--;
        } else {
            h->busy = false;
        }
        portEXIT_CRITICAL_ISR(&h->lock);
        if (!more) return yield;
    }
}

static IRAM_ATTR bool s_on_strip_done(const jpeg_enh_strip_event_t *evt, void *user_ctx)
{
    jpeg_ppa_pipeline_handle_t h = (jpeg_ppa_pipeline_handle_t)user_ctx;
    bool overflow = false;
    portENTER_CRITICAL_ISR(&h->lock);
    bool start = !h->abort_frame && !h->busy;
    if (start) {
        h->busy = true;
    } else if (!h->abort_frame) {
        if (h->parked_count < MAX_PARKED) {
            h->parked[(h->parked_head + h->parked_count) % MAX_PARKED] = *evt;
            h->parked_count++;
        } else {
            overflow = true;
        }
    }
    portEXIT_CRITICAL_ISR(&h->lock);
    if (start) return s_pump(h, *evt);
    if (overflow) {
        s_set_frame_err(h, ESP_ERR_INVALID_STATE);
        return s_finish_strip(h, evt->strip_idx);
    }
    return false;
}

static IRAM_ATTR bool s_on_srm_done(void *ctx)
{
    jpeg_ppa_pipeline_handle_t h = (jpeg_ppa_pipeline_handle_t)ctx;
    bool yield = s_finish_strip(h, h->busy_strip);

    jpeg_enh_strip_event_t next;
    portENTER_CRITICAL_ISR(&h->lock);
    bool more = !h->abort_frame && h->parked_count > 0;
    if (more) {
        next = h->parked[h->parked_head];
        h->parked_head = (h->parked_head + 1) % MAX_PARKED;
        h->parked_count--;
    } else {
        h->busy = false;
    }
    portEXIT_CRITICAL_ISR(&h->lock);
    if (more) yield |= s_pump(h, next);
    return yield;
}

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

esp_err_t jpeg_ppa_pipeline_new(const jpeg_ppa_pipeline_cfg_t *cfg,
                                jpeg_ppa_pipeline_handle_t *out_handle)
{
    if (!cfg || !out_handle) return ESP_ERR_INVALID_ARG;
    if (!cfg->strip_bufs[0]) return ESP_ERR_INVALID_ARG;
    jpeg_dec_output_format_t jpeg_fmt = s_jpeg_out_for_strip_cm(cfg->strip_color_mode);
    if ((int)jpeg_fmt == -1) {
        ESP_LOGE(TAG, "unsupported strip color mode 0x%lx", (unsigned long)cfg->strip_color_mode);
        return ESP_ERR_INVALID_ARG;
    }

    jpeg_ppa_pipeline_handle_t h = heap_caps_calloc(1, sizeof(*h), MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    if (!h) return ESP_ERR_NO_MEM;
    h->cfg = *cfg;
    h->lock = (portMUX_TYPE)portMUX_INITIALIZER_UNLOCKED;
    uint32_t timeout_ms = cfg->timeout_ms ? cfg->timeout_ms : DEFAULT_TIMEOUT_MS;
    h->timeout = pdMS_TO_TICKS(timeout_ms);

    h->all_done = xSemaphoreCreateBinary();
    if (!h->all_done) {
        jpeg_ppa_pipeline_del(h);
        return ESP_ERR_NO_MEM;
    }

    esp_err_t err = ppa_srm_fast_new(s_on_srm_done, h, &h->srm);
    if (err != ESP_OK) { jpeg_ppa_pipeline_del(h); return err; }

    jpeg_enh_strip_decoder_cfg_t dcfg = {
        .decode = {
            .output_format  = jpeg_fmt,
            .rgb_order      = cfg->rgb_order,
            .conv_std       = cfg->conv_std,
            .yuv_full_range = cfg->yuv_full_range,
        },
        .strip_bufs       = { cfg->strip_bufs[0], cfg->strip_bufs[1] },
        .strip_buf_size   = cfg->strip_buf_size,
        .timeout_ms       = timeout_ms,
        .on_frame_start   = s_on_frame_start,
        .on_strip_done    = s_on_strip_done,
        .user_ctx         = h,
    };
    err = jpeg_enh_strip_decoder_new(&dcfg, &h->decoder);
    if (err != ESP_OK) { jpeg_ppa_pipeline_del(h); return err; }

    *out_handle = h;
    return ESP_OK;
}

esp_err_t jpeg_ppa_pipeline_del(jpeg_ppa_pipeline_handle_t h)
{
    if (!h) return ESP_OK;
    if (h->decoder)  { jpeg_enh_strip_decoder_del(h->decoder); h->decoder = NULL; }
    if (h->srm)      { ppa_srm_fast_del(h->srm); h->srm = NULL; }
    if (h->all_done) { vSemaphoreDelete(h->all_done); h->all_done = NULL; }
    heap_caps_free(h);
    return ESP_OK;
}

jpeg_enh_strip_decoder_handle_t jpeg_ppa_pipeline_get_decoder(jpeg_ppa_pipeline_handle_t h)
{
    return h ? h->decoder : NULL;
}

esp_err_t jpeg_ppa_pipeline_process(jpeg_ppa_pipeline_handle_t h,
                                    const void *jpeg_data, size_t jpeg_size,
                                    const jpeg_ppa_output_t *out,
                                    const jpeg_ppa_transform_t *transform,
                                    jpeg_enh_frame_info_t *info)
{
    if (!h || !h->decoder || !jpeg_data || !out || !out->buffer) return ESP_ERR_INVALID_ARG;
    if (out->pic_w == 0 || out->pic_h == 0) return ESP_ERR_INVALID_ARG;
    if (h->broken) return ESP_ERR_INVALID_STATE;

    h->cur.valid = false;
    h->cur.out = *out;
    if (h->cur.out.buffer_size == 0) {
        h->cur.out.buffer_size = (size_t)out->pic_w * out->pic_h * s_cm_bits(out->color_mode) / 8;
    }
    h->cur.t = transform ? *transform : (jpeg_ppa_transform_t){ 0 };
    if (h->cur.t.scale_x == 0.0f) h->cur.t.scale_x = 1.0f;
    if (h->cur.t.scale_y == 0.0f) h->cur.t.scale_y = 1.0f;

    h->pending_strips = 0;
    h->frame_err = ESP_OK;
    h->abort_frame = false;
    h->busy = false;
    h->parked_head = h->parked_count = 0;
    xSemaphoreTake(h->all_done, 0);

    h->in_process = true;
    esp_err_t err = jpeg_enh_strip_decoder_process(h->decoder, (const uint8_t *)jpeg_data,
                                                   (uint32_t)jpeg_size, info);
    h->in_process = false;
    if (!h->cur.valid) return err;

    if (err != ESP_OK) {
        // Decode failed: strips that never arrived or are still parked will
        // never reach PPA; settle them and wait out the one in flight.
        portENTER_CRITICAL(&h->lock);
        h->abort_frame = true;
        uint32_t parked = h->parked_count;
        h->parked_count = 0;
        portEXIT_CRITICAL(&h->lock);
        uint32_t settle = parked + h->cur.frame.strip_count
                          - jpeg_enh_strip_decoder_strips_delivered(h->decoder);
        if (settle && __atomic_sub_fetch(&h->pending_strips, settle, __ATOMIC_ACQ_REL) == 0) {
            xSemaphoreGive(h->all_done);
        }
    }

    if (xSemaphoreTake(h->all_done, h->timeout) != pdTRUE) {
        ESP_LOGE(TAG, "PPA did not finish (%lu strips left)", (unsigned long)h->pending_strips);
        portENTER_CRITICAL(&h->lock);
        h->abort_frame = true;
        portEXIT_CRITICAL(&h->lock);
        if (ppa_srm_fast_abort(h->srm) != ESP_OK) h->broken = true;
        if (err == ESP_OK) err = ESP_ERR_TIMEOUT;
    }
    ppa_srm_fast_end_frame(h->srm);
    if (err == ESP_OK && h->frame_err != ESP_OK) {
        ESP_LOGW(TAG, "ppa strip err=%s", esp_err_to_name(h->frame_err));
        err = h->frame_err;
    }
    return err;
}
