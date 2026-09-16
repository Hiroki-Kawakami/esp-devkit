/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * jpeg_decode_enhanced — PPA SRM submission path (device).
 *
 * Mirrors IDF's ppa_do_scale_rotate_mirror() / ppa_srm_transaction_on_picked()
 * but drops the per-call validation, cache sync, client queue and task
 * round-trip. Mutual exclusion with regular PPA SRM clients uses the driver's
 * own engine semaphore, which is only reachable through ppa_priv.h.
 */

#include <string.h>
#include "ppa_srm_fast.h"

#include "esp_idf_version.h"
#if !defined(JPEG_DECODE_ENHANCED_SKIP_IDF_VERSION_CHECK)
#if ESP_IDF_VERSION < ESP_IDF_VERSION_VAL(6, 0, 0) || ESP_IDF_VERSION >= ESP_IDF_VERSION_VAL(6, 2, 0)
#error "ppa_srm_fast reaches into esp_driver_ppa private structs (ppa_priv.h) and has been validated against ESP-IDF v6.0.x ~ v6.1.x. Re-validate the private struct layout for this IDF version, then define JPEG_DECODE_ENHANCED_SKIP_IDF_VERSION_CHECK."
#endif
#endif

#include "ppa_priv.h"
#include "esp_private/dma2d.h"
#include "hal/ppa_ll.h"
#include "hal/dma2d_ll.h"
#include "hal/color_hal.h"
#include "hal/cache_hal.h"
#include "hal/cache_ll.h"
#include "soc/dma2d_channel.h"
#include "esp_cache.h"
#include "esp_memory_utils.h"
#include "esp_heap_caps.h"
#include "esp_attr.h"
#include "esp_log.h"

static const char *TAG = "ppa_srm_fast";

struct ppa_srm_fast_s {
    ppa_client_handle_t client;         // keeps the SRM engine powered; its engine sem is our lock
    ppa_engine_t *engine;
    ppa_platform_t *platform;
    ppa_dev_t *dev;
    dma2d_descriptor_t *tx_desc;
    dma2d_descriptor_t *rx_desc;
    dma2d_trans_config_t trans;
    dma2d_trans_t *trans_placeholder;
    ppa_srm_fast_done_cb_t cb;
    void *cb_ctx;

    // Frame-constant state latched by begin_frame()
    ppa_srm_oper_config_t op;
    ppa_srm_color_mode_t hw_in_cm;      // YUV444 input is converted to RGB888 by 2D-DMA
    uint32_t in_pbyte;
    uint32_t out_bits;
    uint32_t sx_int, sx_frag, sy_int, sy_frag;
    uint32_t dscr_block_h, dscr_block_v;
    uint32_t mb_h;
    size_t out_sync_len;                // 0 = output buffer not cached

    bool bypass_mb_order;               // per strip, consumed by on_job_picked
    bool locked;
};

static IRAM_ATTR bool s_on_recv_eof(dma2d_channel_handle_t chan, dma2d_event_data_t *evt, void *user_data)
{
    ppa_srm_fast_handle_t h = user_data;
    return h->cb(h->cb_ctx);
}

static IRAM_ATTR bool s_on_job_picked(uint32_t num_chans, const dma2d_trans_channel_info_t *chans, void *user_config)
{
    ppa_srm_fast_handle_t h = user_config;
    ppa_dev_t *dev = h->dev;
    const ppa_srm_oper_config_t *op = &h->op;

    ppa_ll_srm_reset(dev);

    dma2d_channel_handle_t tx = NULL, rx = NULL;
    for (uint32_t i = 0; i < num_chans; i++) {
        if (chans[i].dir == DMA2D_CHANNEL_DIRECTION_TX) tx = chans[i].chan;
        else rx = chans[i].chan;
    }

    dma2d_trigger_t trig = { .periph = DMA2D_TRIG_PERIPH_PPA_SRM, .periph_sel_id = SOC_DMA2D_TRIG_PERIPH_PPA_SRM_TX };
    dma2d_connect(tx, &trig);
    trig.periph_sel_id = SOC_DMA2D_TRIG_PERIPH_PPA_SRM_RX;
    dma2d_connect(rx, &trig);

    dma2d_transfer_ability_t ability = {
        .access_ext_mem = true,
        .data_burst_length = h->client->data_burst_length,
        .desc_burst_en = true,
        .mb_size = DMA2D_MACRO_BLOCK_SIZE_NONE,
    };
    dma2d_set_transfer_ability(tx, &ability);
    dma2d_set_transfer_ability(rx, &ability);

    if (op->in.srm_cm == PPA_SRM_COLOR_MODE_YUV444) {
        dma2d_csc_config_t csc = {
            .tx_csc_option = (op->in.yuv_std == PPA_COLOR_CONV_STD_RGB_YUV_BT601)
                             ? DMA2D_CSC_TX_YUV444_TO_RGB888_601 : DMA2D_CSC_TX_YUV444_TO_RGB888_709,
        };
        dma2d_configure_color_space_conversion(tx, &csc);
    }

    dma2d_dscr_port_mode_config_t dscr_port = { .block_h = h->dscr_block_h, .block_v = h->dscr_block_v };
    dma2d_configure_dscr_port_mode(tx, &dscr_port);

    dma2d_rx_event_callbacks_t cbs = { .on_recv_eof = s_on_recv_eof };
    dma2d_register_rx_event_callbacks(rx, &cbs, h);

    dma2d_set_desc_addr(tx, (intptr_t)h->tx_desc);
    dma2d_set_desc_addr(rx, (intptr_t)h->rx_desc);
    dma2d_start(tx);
    dma2d_start(rx);

    ppa_ll_srm_set_rx_color_mode(dev, h->hw_in_cm);
    if (PPA_IS_CM_YUV(h->hw_in_cm)) {
        ppa_ll_srm_set_rx_yuv_range(dev, op->in.yuv_range);
        ppa_ll_srm_set_rx_yuv2rgb_std(dev, op->in.yuv_std);
    }
    ppa_ll_srm_enable_rx_byte_swap(dev, op->byte_swap);
    ppa_ll_srm_enable_rx_rgb_swap(dev, op->rgb_swap);
    ppa_ll_srm_configure_rx_alpha(dev, PPA_ALPHA_NO_CHANGE, 0);

    ppa_ll_srm_set_tx_color_mode(dev, op->out.srm_cm);
    if (PPA_IS_CM_YUV(op->out.srm_cm)) {
        ppa_ll_srm_set_tx_yuv_range(dev, op->out.yuv_range);
        ppa_ll_srm_set_tx_rgb2yuv_std(dev, op->out.yuv_std);
    }

    ppa_ll_srm_set_rotation_angle(dev, op->rotation_angle);
    ppa_ll_srm_set_scaling_x(dev, h->sx_int, h->sx_frag);
    ppa_ll_srm_set_scaling_y(dev, h->sy_int, h->sy_frag);
    ppa_ll_srm_enable_mirror_x(dev, op->mirror_x);
    ppa_ll_srm_enable_mirror_y(dev, op->mirror_y);
#if CONFIG_IDF_TARGET_ESP32P4
    ppa_ll_srm_bypass_mb_order(dev, h->bypass_mb_order);
#endif
    ppa_ll_srm_start(dev);
    return false;
}

esp_err_t ppa_srm_fast_new(ppa_srm_fast_done_cb_t cb, void *ctx, ppa_srm_fast_handle_t *out)
{
    if (!cb || !out) return ESP_ERR_INVALID_ARG;
    ppa_srm_fast_handle_t h = heap_caps_calloc(1, sizeof(*h), MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    if (!h) return ESP_ERR_NO_MEM;
    h->cb = cb;
    h->cb_ctx = ctx;

    ppa_client_config_t pcfg = { .oper_type = PPA_OPERATION_SRM };
    esp_err_t err = ppa_register_client(&pcfg, &h->client);
    if (err != ESP_OK) goto fail;
    h->engine = h->client->engine;
    h->platform = h->engine->platform;
    h->dev = h->platform->hal.dev;

    size_t align = DMA2D_LL_DESC_ALIGNMENT;
    size_t cache_line = cache_hal_get_cache_line_size(CACHE_LL_LEVEL_INT_MEM, CACHE_TYPE_DATA);
    if (cache_line > align) align = cache_line;
    size_t desc_size = h->platform->dma_desc_mem_size;
    h->tx_desc = heap_caps_aligned_calloc(align, 1, desc_size, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    h->rx_desc = heap_caps_aligned_calloc(align, 1, desc_size, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    h->trans_placeholder = heap_caps_calloc(1, SIZEOF_DMA2D_TRANS_T, MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    if (!h->tx_desc || !h->rx_desc || !h->trans_placeholder) { err = ESP_ERR_NO_MEM; goto fail; }

    h->trans = (dma2d_trans_config_t){
        .tx_channel_num = 1,
        .rx_channel_num = 1,
        .user_config = h,
        .on_job_picked = s_on_job_picked,
    };
    *out = h;
    return ESP_OK;
fail:
    ppa_srm_fast_del(h);
    return err;
}

void ppa_srm_fast_del(ppa_srm_fast_handle_t h)
{
    if (!h) return;
    if (h->client) ppa_unregister_client(h->client);
    free(h->tx_desc);
    free(h->rx_desc);
    free(h->trans_placeholder);
    free(h);
}

static inline uint32_t s_scaled(uint32_t v, uint32_t i, uint32_t f)
{
    return v * i + v * f / PPA_LL_SRM_SCALING_FRAG_MAX;
}

static bool s_cm_supports_byte_swap(ppa_srm_color_mode_t cm)
{
    return cm == PPA_SRM_COLOR_MODE_ARGB8888 || cm == PPA_SRM_COLOR_MODE_RGB565;
}

static bool s_cm_supports_rgb_swap(ppa_srm_color_mode_t cm)
{
    return cm == PPA_SRM_COLOR_MODE_ARGB8888 || cm == PPA_SRM_COLOR_MODE_RGB888 || cm == PPA_SRM_COLOR_MODE_RGB565;
}

static esp_err_t s_validate(ppa_srm_fast_handle_t h, const ppa_srm_oper_config_t *op)
{
    if (!op->out.buffer || !(esp_ptr_internal(op->out.buffer) || esp_ptr_external_ram(op->out.buffer))) {
        ESP_LOGE(TAG, "invalid out.buffer addr");
        return ESP_ERR_INVALID_ARG;
    }
    if (!ppa_ll_srm_is_color_mode_supported(op->in.srm_cm) ||
        !ppa_ll_srm_is_color_mode_supported(op->out.srm_cm) || op->out.srm_cm == PPA_SRM_COLOR_MODE_YUV444) {
        ESP_LOGE(TAG, "unsupported color mode");
        return ESP_ERR_INVALID_ARG;
    }
    if (op->in.pic_w > DMA2D_LL_DESC_2D_FIELD_MAX || op->out.pic_w > DMA2D_LL_DESC_2D_FIELD_MAX ||
        op->out.pic_h > DMA2D_LL_DESC_2D_FIELD_MAX) {
        ESP_LOGE(TAG, "dimension exceeds DMA2D descriptor field limit");
        return ESP_ERR_INVALID_ARG;
    }
    if (op->in.block_w == 0 || op->in.block_offset_x >= op->in.pic_w ||
        op->in.block_w > op->in.pic_w - op->in.block_offset_x) {
        ESP_LOGE(TAG, "in.block_w + in.block_offset_x does not fit in the in pic");
        return ESP_ERR_INVALID_ARG;
    }
    if (op->in.srm_cm == PPA_SRM_COLOR_MODE_YUV420 &&
        (op->in.pic_w % 2 || op->in.block_w % 2 || op->in.block_offset_x % 2)) {
        ESP_LOGE(TAG, "YUV420 input does not support odd w/offset_x");
        return ESP_ERR_INVALID_ARG;
    }
    if (op->out.srm_cm == PPA_SRM_COLOR_MODE_YUV420 && (op->out.pic_w % 2 || op->out.pic_h % 2)) {
        ESP_LOGE(TAG, "YUV420 output does not support odd h/w");
        return ESP_ERR_INVALID_ARG;
    }
    h->out_bits = color_hal_pixel_format_fourcc_get_bit_depth((esp_color_fourcc_t)op->out.srm_cm);
    uint64_t out_len = (uint64_t)op->out.pic_w * op->out.pic_h * h->out_bits / 8;
    if (out_len > op->out.buffer_size) {
        ESP_LOGE(TAG, "out.pic_w/h mismatch with out.buffer_size");
        return ESP_ERR_INVALID_ARG;
    }
    if (!(op->scale_x < PPA_LL_SRM_SCALING_INT_MAX && op->scale_x >= (1.0 / PPA_LL_SRM_SCALING_FRAG_MAX) &&
          op->scale_y < PPA_LL_SRM_SCALING_INT_MAX && op->scale_y >= (1.0 / PPA_LL_SRM_SCALING_FRAG_MAX))) {
        ESP_LOGE(TAG, "invalid scale");
        return ESP_ERR_INVALID_ARG;
    }
    if (!ppa_check_buffer_alignment(h->client, &op->out, false, op->out.pic_w)) {
        return ESP_ERR_INVALID_ARG;
    }
    if (op->byte_swap && !s_cm_supports_byte_swap(op->in.srm_cm)) {
        ESP_LOGE(TAG, "in.srm_cm does not support byte_swap");
        return ESP_ERR_INVALID_ARG;
    }
    if (op->rgb_swap && !s_cm_supports_rgb_swap(op->in.srm_cm)) {
        ESP_LOGE(TAG, "in.srm_cm does not support rgb_swap");
        return ESP_ERR_INVALID_ARG;
    }
    return ESP_OK;
}

esp_err_t ppa_srm_fast_begin_frame(ppa_srm_fast_handle_t h, const ppa_srm_oper_config_t *tmpl)
{
    if (!h || !tmpl) return ESP_ERR_INVALID_ARG;
    if (h->locked) return ESP_ERR_INVALID_STATE;

    esp_err_t err = s_validate(h, tmpl);
    if (err != ESP_OK) return err;

    h->op = *tmpl;
    h->in_pbyte = dma2d_desc_pixel_format_to_pbyte_value((esp_color_fourcc_t)tmpl->in.srm_cm);
    h->hw_in_cm = (tmpl->in.srm_cm == PPA_SRM_COLOR_MODE_YUV444) ? PPA_SRM_COLOR_MODE_RGB888 : tmpl->in.srm_cm;
    h->trans.channel_flags = (tmpl->in.srm_cm == PPA_SRM_COLOR_MODE_YUV444) ? DMA2D_CHANNEL_FUNCTION_FLAG_TX_CSC : 0;

    h->sx_int = (uint32_t)tmpl->scale_x;
    h->sx_frag = (uint32_t)(tmpl->scale_x * PPA_LL_SRM_SCALING_FRAG_MAX) & (PPA_LL_SRM_SCALING_FRAG_MAX - 1);
    h->sy_int = (uint32_t)tmpl->scale_y;
    h->sy_frag = (uint32_t)(tmpl->scale_y * PPA_LL_SRM_SCALING_FRAG_MAX) & (PPA_LL_SRM_SCALING_FRAG_MAX - 1);
    if (tmpl->out.srm_cm == PPA_SRM_COLOR_MODE_YUV420) {
        h->sx_frag &= ~1u;
        h->sy_frag &= ~1u;
    } else if (PPA_IS_CM_YUV422(tmpl->out.srm_cm)) {
        h->sx_frag &= ~1u;
    }

    ppa_ll_srm_mb_size_t mb = ppa_ll_srm_get_mb_size(h->dev);
    h->mb_h = (mb == PPA_LL_SRM_MB_SIZE_16_16) ? 16 : 32;
    ppa_ll_srm_get_dma_dscr_port_mode_block_size(h->dev, h->hw_in_cm, mb, &h->dscr_block_h, &h->dscr_block_v);

    size_t align = esp_ptr_external_ram(tmpl->out.buffer) ? h->platform->ext_mem_align : h->platform->int_mem_align;
    h->out_sync_len = align ? tmpl->out.buffer_size : 0;

    xSemaphoreTake(h->engine->sem, portMAX_DELAY);
#if CONFIG_PM_ENABLE
    if (h->engine->pm_lock) esp_pm_lock_acquire(h->engine->pm_lock);
#endif
    h->locked = true;

    if (h->out_sync_len) {
        esp_cache_msync(tmpl->out.buffer, h->out_sync_len, ESP_CACHE_MSYNC_FLAG_DIR_M2C);
    }
    return ESP_OK;
}

IRAM_ATTR esp_err_t ppa_srm_fast_submit(ppa_srm_fast_handle_t h, void *in_buf, uint32_t in_pic_h,
                                        uint32_t in_offset_y, uint32_t in_block_h,
                                        uint32_t out_offset_x, uint32_t out_offset_y)
{
    const ppa_srm_oper_config_t *op = &h->op;
    if (in_block_h == 0 || in_pic_h > DMA2D_LL_DESC_2D_FIELD_MAX ||
        in_offset_y >= in_pic_h || in_block_h > in_pic_h - in_offset_y) {
        return ESP_ERR_INVALID_ARG;
    }
    if (op->in.srm_cm == PPA_SRM_COLOR_MODE_YUV420 && ((in_pic_h | in_offset_y | in_block_h) & 1)) {
        return ESP_ERR_INVALID_ARG;
    }
    if (op->out.srm_cm == PPA_SRM_COLOR_MODE_YUV420 && ((out_offset_x | out_offset_y) & 1)) {
        return ESP_ERR_INVALID_ARG;
    }

    uint32_t in_w = op->in.block_w;
    uint32_t out_w, out_h;
    if (op->rotation_angle == PPA_SRM_ROTATION_ANGLE_0 || op->rotation_angle == PPA_SRM_ROTATION_ANGLE_180) {
        out_w = s_scaled(in_w, h->sx_int, h->sx_frag);
        out_h = s_scaled(in_block_h, h->sy_int, h->sy_frag);
    } else {
        out_w = s_scaled(in_block_h, h->sy_int, h->sy_frag);
        out_h = s_scaled(in_w, h->sx_int, h->sx_frag);
    }
    if (out_w == 0 || out_h == 0 ||
        out_offset_x >= op->out.pic_w || out_w > op->out.pic_w - out_offset_x ||
        out_offset_y >= op->out.pic_h || out_h > op->out.pic_h - out_offset_y) {
        return ESP_ERR_INVALID_ARG;
    }

    dma2d_descriptor_t *t = h->tx_desc;
    t->vb_size = in_block_h;
    t->hb_length = in_w;
    t->err_eof = 0;
    t->dma2d_en = 1;
    t->suc_eof = 1;
    t->owner = DMA2D_DESCRIPTOR_BUFFER_OWNER_DMA;
    t->va_size = in_pic_h;
    t->ha_length = op->in.pic_w;
    t->pbyte = h->in_pbyte;
    t->y = in_offset_y;
    t->x = op->in.block_offset_x;
    t->mode = DMA2D_DESCRIPTOR_BLOCK_RW_MODE_SINGLE;
    t->buffer = in_buf;
    t->next = NULL;

    // vb/hb are written back by the hardware, but 0 raises DESC_ERROR and
    // YUV420 needs even values.
    dma2d_descriptor_t *r = h->rx_desc;
    r->vb_size = 2;
    r->hb_length = 2;
    r->err_eof = 0;
    r->dma2d_en = 1;
    r->suc_eof = 1;
    r->owner = DMA2D_DESCRIPTOR_BUFFER_OWNER_DMA;
    r->va_size = op->out.pic_h;
    r->ha_length = op->out.pic_w;
    r->y = out_offset_y;
    r->x = out_offset_x;
    r->mode = DMA2D_DESCRIPTOR_BLOCK_RW_MODE_SINGLE;
    r->buffer = op->out.buffer;
    r->next = NULL;

    esp_cache_msync(t, h->platform->dma_desc_mem_size, ESP_CACHE_MSYNC_FLAG_DIR_C2M);
    esp_cache_msync(r, h->platform->dma_desc_mem_size, ESP_CACHE_MSYNC_FLAG_DIR_C2M);

#if CONFIG_IDF_TARGET_ESP32P4
    // Hardware bug workaround (DIG-734), same as ppa_srm_transaction_on_picked()
    uint32_t w_out = s_scaled(in_w, h->sx_int, h->sx_frag);
    uint32_t w_div = (op->out.srm_cm == PPA_SRM_COLOR_MODE_ARGB8888 || op->out.srm_cm == PPA_SRM_COLOR_MODE_RGB888) ? 32 : 64;
    uint32_t w_left = w_out % w_div;
    if (w_left == 0) w_left = w_div;
    uint32_t h_in_left = in_block_h % h->mb_h;
    if (h_in_left == 0) h_in_left = h->mb_h;
    uint32_t h_left = s_scaled(h_in_left, h->sy_int, h->sy_frag);
    h->bypass_mb_order = ((w_out > w_div) || (in_block_h > h->mb_h)) &&
                         (w_left * h_left * h->out_bits < 12 * 128);
#endif

    return dma2d_enqueue(h->platform->dma2d_pool_handle, &h->trans, h->trans_placeholder);
}

esp_err_t ppa_srm_fast_abort(ppa_srm_fast_handle_t h)
{
    if (!h) return ESP_ERR_INVALID_ARG;
    bool need_yield;
    esp_err_t err = dma2d_force_end(h->trans_placeholder, &need_yield);
    if (err != ESP_OK) ESP_LOGE(TAG, "cannot cancel a queued SRM submission: %s", esp_err_to_name(err));
    return err;
}

void ppa_srm_fast_end_frame(ppa_srm_fast_handle_t h)
{
    if (!h || !h->locked) return;
    if (h->out_sync_len) {
        esp_cache_msync(h->op.out.buffer, h->out_sync_len, ESP_CACHE_MSYNC_FLAG_DIR_M2C);
    }
#if CONFIG_PM_ENABLE
    if (h->engine->pm_lock) esp_pm_lock_release(h->engine->pm_lock);
#endif
    h->locked = false;

    // A non-blocking client that queued while we held the engine found the
    // semaphore taken and left its transaction for the in-flight one's done
    // ISR to start. There is no such ISR, so start it here the way
    // ppa_transaction_done_cb() would, keeping the semaphore for it.
    portENTER_CRITICAL(&h->engine->spinlock);
    ppa_trans_t *next = STAILQ_FIRST(&h->engine->trans_stailq);
    portEXIT_CRITICAL(&h->engine->spinlock);
    if (next) {
#if CONFIG_PM_ENABLE
        if (h->engine->pm_lock) esp_pm_lock_acquire(h->engine->pm_lock);
#endif
        dma2d_enqueue(h->platform->dma2d_pool_handle, next->trans_desc, next->dma_trans_placeholder);
    } else {
        xSemaphoreGive(h->engine->sem);
    }
}
