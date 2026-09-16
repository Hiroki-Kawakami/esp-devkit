/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * jpeg_decode_enhanced — Layer 1 implementation.
 *
 * Replays the bulk of the IDF jpeg_decoder_process() flow but writes the
 * decoded image into one or two caller-owned strip buffers via linked 2D-DMA
 * RX descriptors. Each finished strip fires on_strip_done() from ISR context so
 * a downstream consumer (PPA SRM, CPU, ...) can start processing it
 * immediately, in parallel with the decode of subsequent strips.
 *
 * Bandwidth motivation: when the JPEG codec writes into PSRAM and a consumer
 * later reads the same data back from PSRAM, the round-trip dominates the
 * PSRAM bandwidth budget on ESP32-P4 and limits 1280x720 throughput to
 * ~20fps. Keeping the intermediate raster in internal SRAM removes that
 * round-trip. (PSRAM strip buffers are still accepted for callers that only
 * want the smaller intermediate footprint.)
 *
 * Hard-won 2D-DMA facts this implementation is built on:
 *  1. Owner-bit backpressure does NOT work for JPEG-RX: a CPU-owned
 *     descriptor with in_check_owner raises RX_DSCR_ERROR and ends the
 *     transaction; it does not pause.
 *  2. End-of-chain (next=NULL) pauses the RX until dma2d_append(); a slow
 *     consumer only runs into the frame timeout.
 *  3. Backpressure is dynamic chain extension via dma2d_append(): pre-link
 *     only ring_count descriptors and splice in the descriptor for strip
 *     i + ring_count when the consumer releases strip i.
 *  3a. Descriptors are recycled from a pool of RX_DESC_POOL entries. The pool
 *     must hold at least ring_count + 1: with ring_count entries the
 *     R=1 case links a descriptor to itself and trips the dma2d ISR's
 *     FSM-idle assert.
 *  4. suc_eof=0 on every descriptor: the JPEG-DMA bridge fires SUC_EOF off
 *     the JPEG hardware's frame-done signal, not off the descriptor eof bit.
 *  5. on_recv_eof must post JPEG_DMA2D_RX_EOF into the engine's evt_queue to
 *     unblock the wait loop (mirroring IDF's jpeg_rx_eof).
 *  6. on_desc_done does not populate rx_eof_desc_addr; strips are counted
 *     with an in-order counter.
 */

#include <stdlib.h>
#include <string.h>
#include "jpeg_decode_enhanced.h"

#include "esp_idf_version.h"
#if !defined(JPEG_DECODE_ENHANCED_SKIP_IDF_VERSION_CHECK)
#if ESP_IDF_VERSION < ESP_IDF_VERSION_VAL(6, 0, 0) || ESP_IDF_VERSION >= ESP_IDF_VERSION_VAL(6, 2, 0)
#error "jpeg_decode_enhanced reaches into esp_driver_jpeg private structs (jpeg_private.h) and has been validated against ESP-IDF v6.0.x ~ v6.1.x. Re-validate the private struct layout for this IDF version, then define JPEG_DECODE_ENHANCED_SKIP_IDF_VERSION_CHECK."
#endif
#endif

#include "jpeg_private.h"
#include "private/jpeg_parse_marker.h"
#include "private/jpeg_param.h"
#include "esp_private/dma2d.h"
#include "hal/jpeg_ll.h"
#include "hal/jpeg_hal.h"
#include "hal/jpeg_defs.h"
#include "hal/cache_ll.h"
#include "hal/cache_hal.h"
#include "hal/dma2d_ll.h"
#include "hal/color_hal.h"
#include "soc/dma2d_channel.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "esp_cache.h"
#include "esp_private/esp_cache_private.h"
#include "esp_memory_utils.h"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "esp_attr.h"

static const char *TAG = "jpeg_enh";

#define DEFAULT_TIMEOUT_MS   200
#define STRIP_BUF_ALIGN      64
// PPA SRM processes 16-row macro blocks (a shorter strip costs a full block)
// and quantizes scale to 1/16, so strips land on 16-row boundaries.
#define STRIP_ROW_ALIGN      16
#define MAX_RING_COUNT       2
#define RX_DESC_POOL         (MAX_RING_COUNT + 1)

struct jpeg_enh_strip_decoder_s {
    jpeg_decoder_handle_t engine;       // IDF JPEG decoder engine (owns DMA pool, mutex, ISR)
    jpeg_enh_strip_decoder_cfg_t cfg;   // copy of user config

    // Caller-owned strip buffers; ring_count == 0 means whole-frame API only
    void *strip_bufs[MAX_RING_COUNT];
    uint32_t ring_count;
    size_t strip_buf_size;              // usable bytes per buffer (cache-line aligned)

    dma2d_descriptor_t *rxlinks;        // RX_DESC_POOL entries, recycled per strip
    dma2d_descriptor_t *txlink;
    size_t desc_byte_size;              // size of one descriptor (cache aligned)

    SemaphoreHandle_t frame_done_sem;   // released by on_recv_eof
    portMUX_TYPE spin;                  // protects chain bookkeeping + frame_active

    // Per-frame state (valid while the codec mutex is held by process())
    jpeg_enh_frame_info_t frame;        // geometry of the frame in flight
    uint32_t last_strip_rows;           // padded rows of the final strip
    bool whole_frame;                   // jpeg_enh_decoder_process() path (no strip callbacks)
    volatile bool frame_active;         // gates release_strip() splices
    dma2d_channel_handle_t rx_chan;     // captured in on_job_picked, used by release_strip()
    volatile uint32_t isr_next_strip;   // next strip index expected by on_desc_done (in-order)
    volatile uint32_t chain_tail;       // index of the last descriptor currently linked

    uint32_t dma_hb;                    // horizontal block size (pixels) for the RX descriptors
    uint32_t dma_vb;                    // vertical block size (= mcu_h)
};

static inline uint32_t s_align_up(uint32_t v, uint32_t a) { return (v + a - 1) / a * a; }

static inline uint32_t s_bits_per_pixel(jpeg_dec_output_format_t fmt)
{
    return color_hal_pixel_format_fourcc_get_bit_depth(fmt);
}

// =============================================================================
// JPEG header parsing helpers (replicas of static helpers in jpeg_decode.c)
// =============================================================================

static esp_err_t s_jpeg_default_huff_table(jpeg_dec_header_info_t *header_info)
{
    memcpy(header_info->huffbits[0][0], luminance_dc_coefficients, JPEG_HUFFMAN_BITS_LEN_TABLE_LEN);
    memcpy(header_info->huffbits[0][1], chrominance_dc_coefficients, JPEG_HUFFMAN_BITS_LEN_TABLE_LEN);
    memcpy(header_info->huffbits[1][0], luminance_ac_coefficients, JPEG_HUFFMAN_BITS_LEN_TABLE_LEN);
    memcpy(header_info->huffbits[1][1], chrominance_ac_coefficients, JPEG_HUFFMAN_BITS_LEN_TABLE_LEN);
    memcpy(header_info->huffcode[0][0], luminance_dc_values, JPEG_HUFFMAN_DC_VALUE_TABLE_LEN);
    memcpy(header_info->huffcode[0][1], chrominance_dc_values, JPEG_HUFFMAN_DC_VALUE_TABLE_LEN);
    memcpy(header_info->huffcode[1][0], luminance_ac_values, JPEG_HUFFMAN_AC_VALUE_TABLE_LEN);
    memcpy(header_info->huffcode[1][1], chrominance_ac_values, JPEG_HUFFMAN_AC_VALUE_TABLE_LEN);
    return ESP_OK;
}

static esp_err_t s_parse_jpeg(jpeg_decoder_handle_t engine, const uint8_t *in_buf, uint32_t inbuf_len)
{
    jpeg_dec_header_info_t *header_info = engine->header_info;
    jpeg_hal_context_t *hal = &engine->codec_base->hal;

    memset(header_info, 0, sizeof(*header_info));
    header_info->buffer_offset = (uint8_t *)in_buf;
    header_info->buffer_left = inbuf_len;
    engine->total_size = inbuf_len;
    header_info->header_size = 0;

    jpeg_ll_soft_rst(hal->dev);
    jpeg_ll_set_codec_mode(hal->dev, JPEG_CODEC_DECODER);
    jpeg_ll_set_picture_height(hal->dev, 0);
    jpeg_ll_set_picture_width(hal->dev, 0);

    const uint8_t *buf_end = in_buf + inbuf_len;
    while (header_info->buffer_left) {
        // Detect underflow / runaway: the IDF marker handlers advance
        // buffer_offset by a length field read from the stream, then
        // decrement buffer_left. A corrupt MJPEG (e.g. dropped USB isoc
        // packet) can make that length huge, wrapping buffer_left around
        // 0 and walking buffer_offset off the end of PSRAM. The invariants
        // below hold for a clean parse; if either fails we've left the
        // buffer and must bail before reading unmapped memory.
        if (header_info->buffer_left > inbuf_len ||
            header_info->buffer_offset < in_buf ||
            header_info->buffer_offset >= buf_end) {
            ESP_LOGW(TAG, "header parse out of bounds (corrupt JPEG)");
            return ESP_ERR_INVALID_STATE;
        }
        uint8_t lastchar = jpeg_get_bytes(header_info, 1);
        uint8_t thischar = jpeg_get_bytes(header_info, 1);
        uint16_t marker = (lastchar << 8 | thischar);
        switch (marker) {
        case JPEG_M_SOI:
            break;
        case JPEG_M_APP0: case JPEG_M_APP1: case JPEG_M_APP2: case JPEG_M_APP3:
        case JPEG_M_APP4: case JPEG_M_APP5: case JPEG_M_APP6: case JPEG_M_APP7:
        case JPEG_M_APP8: case JPEG_M_APP9: case JPEG_M_APP10: case JPEG_M_APP11:
        case JPEG_M_APP12: case JPEG_M_APP13: case JPEG_M_APP14: case JPEG_M_APP15:
            jpeg_parse_appn_marker(header_info);
            break;
        case JPEG_M_COM:
            jpeg_parse_com_marker(header_info);
            break;
        case JPEG_M_DQT:
            jpeg_parse_dqt_marker(header_info);
            break;
        case JPEG_M_SOF0:
            if (jpeg_parse_sof_marker(header_info) != ESP_OK) return ESP_ERR_INVALID_STATE;
            break;
        case JPEG_M_SOF1: case JPEG_M_SOF2: case JPEG_M_SOF3: case JPEG_M_SOF5:
        case JPEG_M_SOF6: case JPEG_M_SOF7: case JPEG_M_SOF9: case JPEG_M_SOF10:
        case JPEG_M_SOF11: case JPEG_M_SOF13: case JPEG_M_SOF14: case JPEG_M_SOF15:
            ESP_LOGE(TAG, "Only baseline-DCT JPEG is supported");
            return ESP_ERR_NOT_SUPPORTED;
        case JPEG_M_DRI:
            jpeg_parse_dri_marker(header_info);
            break;
        case JPEG_M_DHT:
            jpeg_parse_dht_marker(header_info);
            break;
        case JPEG_M_SOS:
            jpeg_parse_sos_marker(header_info);
            break;
        case JPEG_M_INV:
            jpeg_parse_inv_marker(header_info);
            break;
        }
        if (marker == JPEG_M_SOS) break;
    }

    header_info->buffer_left = engine->total_size - header_info->header_size;

    if (!header_info->dht_marker) {
        s_jpeg_default_huff_table(header_info);
    }
    return ESP_OK;
}

static esp_err_t s_apply_header_to_hw(jpeg_decoder_handle_t engine)
{
    jpeg_dec_header_info_t *header_info = engine->header_info;
    jpeg_hal_context_t *hal = &engine->codec_base->hal;

    for (int i = 0; i < header_info->qt_tbl_num; i++) {
        dqt_func[i](hal->dev, header_info->qt_tbl[i]);
    }
    jpeg_ll_set_picture_height(hal->dev, header_info->process_v);
    jpeg_ll_set_picture_width(hal->dev, header_info->process_h);
    jpeg_ll_set_decode_component_num(hal->dev, header_info->nf);
    for (int i = 0; i < header_info->nf; i++) {
        sof_func[i](hal->dev, header_info->ci[i], header_info->hi[i], header_info->vi[i], header_info->qtid[i]);
    }
    if (header_info->nf == 3) {
        switch (header_info->hivi[0]) {
        case 0x11: engine->sample_method = JPEG_DOWN_SAMPLING_YUV444; break;
        case 0x21: engine->sample_method = JPEG_DOWN_SAMPLING_YUV422; break;
        case 0x22: engine->sample_method = JPEG_DOWN_SAMPLING_YUV420; break;
        default: return ESP_ERR_INVALID_STATE;
        }
    } else if (header_info->nf == 1) {
        engine->sample_method = JPEG_DOWN_SAMPLING_GRAY;
    }

    engine->no_color_conversion = ((uint32_t)engine->sample_method == (uint32_t)engine->output_format);

    dht_func[0][0](hal, header_info->huffbits[0][0], header_info->huffcode[0][0], header_info->tmp_huff);
    dht_func[0][1](hal, header_info->huffbits[0][1], header_info->huffcode[0][1], header_info->tmp_huff);
    dht_func[1][0](hal, header_info->huffbits[1][0], header_info->huffcode[1][0], header_info->tmp_huff);
    dht_func[1][1](hal, header_info->huffbits[1][1], header_info->huffcode[1][1], header_info->tmp_huff);

    jpeg_ll_set_restart_interval(hal->dev, header_info->ri);
    return ESP_OK;
}

// =============================================================================
// Descriptor configuration
// =============================================================================

static inline jpeg_dec_format_hb_t s_best_hb_idx(jpeg_dec_output_format_t out_fmt, bool no_csc)
{
    if (no_csc) return JPEG_DEC_DIRECT_OUTPUT_HB;
    switch (out_fmt) {
    case JPEG_DECODE_OUT_FORMAT_RGB888: return JPEG_DEC_RGB888_HB;
    case JPEG_DECODE_OUT_FORMAT_RGB565: return JPEG_DEC_RGB565_HB;
    case JPEG_DECODE_OUT_FORMAT_GRAY:   return JPEG_DEC_GRAY_HB;
    case JPEG_DECODE_OUT_FORMAT_YUV444: return JPEG_DEC_YUV444_HB;
    default: return JPEG_DEC_BEST_HB_MAX;
    }
}

static inline uint8_t s_sample_idx(jpeg_down_sampling_type_t s)
{
    switch (s) {
    case JPEG_DOWN_SAMPLING_YUV444: return 0;
    case JPEG_DOWN_SAMPLING_YUV422: return 1;
    case JPEG_DOWN_SAMPLING_YUV420: return 2;
    case JPEG_DOWN_SAMPLING_GRAY:   return 3;
    default: return 0;
    }
}

static inline dma2d_descriptor_t *s_rx_desc(jpeg_enh_strip_decoder_handle_t h, uint32_t i)
{
    return (dma2d_descriptor_t *)((uint8_t *)h->rxlinks + i * h->desc_byte_size);
}

static inline dma2d_descriptor_t *s_strip_desc(jpeg_enh_strip_decoder_handle_t h, uint32_t strip_idx)
{
    return s_rx_desc(h, strip_idx % RX_DESC_POOL);
}

static void s_fill_tx_descriptor(jpeg_enh_strip_decoder_handle_t h)
{
    jpeg_dec_header_info_t *hi = h->engine->header_info;
    h->txlink->dma2d_en      = JPEG_DMA2D_2D_DISABLE;
    h->txlink->mode          = DMA2D_DESCRIPTOR_BLOCK_RW_MODE_SINGLE;
    h->txlink->vb_size       = hi->buffer_left & JPEG_DMA2D_MAX_SIZE;
    h->txlink->hb_length     = hi->buffer_left & JPEG_DMA2D_MAX_SIZE;
    h->txlink->pbyte         = 1;
    h->txlink->suc_eof       = JPEG_DMA2D_EOF_NOT_LAST;
    h->txlink->owner         = DMA2D_DESCRIPTOR_BUFFER_OWNER_DMA;
    h->txlink->va_size       = (hi->buffer_left >> JPEG_DMA2D_1D_HIGH_14BIT);
    h->txlink->ha_length     = (hi->buffer_left >> JPEG_DMA2D_1D_HIGH_14BIT);
    h->txlink->buffer        = hi->buffer_offset;
    h->txlink->next          = NULL;
    h->txlink->err_eof       = 0;
    h->txlink->x = h->txlink->y = 0;

    esp_cache_msync(h->txlink, h->desc_byte_size, ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED | ESP_CACHE_MSYNC_FLAG_INVALIDATE);
}

static void s_compute_dma_blocks(jpeg_enh_strip_decoder_handle_t h)
{
    jpeg_dec_header_info_t *hi = h->engine->header_info;
    h->engine->bit_per_pixel = color_hal_pixel_format_fourcc_get_bit_depth(h->engine->output_format);

    jpeg_dec_format_hb_t hb_idx = s_best_hb_idx(h->engine->output_format, h->engine->no_color_conversion);
    h->dma_hb = dec_hb_tbl[s_sample_idx(h->engine->sample_method)][hb_idx];
    h->dma_vb = hi->mcuy;
}

// suc_eof=0 on every descriptor: the JPEG-DMA bridge drives SUC_EOF off the
// JPEG hardware's frame-done signal, matching IDF's own decoder.
static IRAM_ATTR void s_fill_strip_descriptor(jpeg_enh_strip_decoder_handle_t h, uint32_t strip_idx)
{
    jpeg_dec_header_info_t *hi = h->engine->header_info;
    dma2d_descriptor_t *d = s_strip_desc(h, strip_idx);

    d->dma2d_en   = JPEG_DMA2D_2D_ENABLE;
    d->mode       = DMA2D_DESCRIPTOR_BLOCK_RW_MODE_MULTIPLE;
    d->vb_size    = h->dma_vb;
    d->hb_length  = h->dma_hb;
    d->pbyte      = dma2d_desc_pixel_format_to_pbyte_value(h->engine->output_format);
    d->suc_eof    = JPEG_DMA2D_EOF_NOT_LAST;
    d->owner      = DMA2D_DESCRIPTOR_BUFFER_OWNER_DMA;
    d->va_size    = (strip_idx + 1 == h->frame.strip_count) ? h->last_strip_rows : h->frame.strip_h;
    d->ha_length  = hi->process_h;
    d->buffer     = h->strip_bufs[strip_idx % h->ring_count];
    d->next       = NULL;
    d->err_eof    = 0;
    d->x = d->y   = 0;
}

static void s_init_strip_descriptors(jpeg_enh_strip_decoder_handle_t h)
{
    uint32_t linked = h->ring_count < h->frame.strip_count ? h->ring_count : h->frame.strip_count;

    h->isr_next_strip = 0;
    for (uint32_t i = 0; i < linked; i++) {
        s_fill_strip_descriptor(h, i);
        if (i > 0) s_strip_desc(h, i - 1)->next = s_strip_desc(h, i);
    }
    h->chain_tail = linked - 1;

    esp_cache_msync(h->rxlinks, h->desc_byte_size * RX_DESC_POOL, ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED | ESP_CACHE_MSYNC_FLAG_INVALIDATE);
    s_fill_tx_descriptor(h);
}

// Single descriptor covering the whole frame (jpeg_enh_decoder_process path).
static void s_init_whole_frame_descriptor(jpeg_enh_strip_decoder_handle_t h, void *out_buf)
{
    jpeg_dec_header_info_t *hi = h->engine->header_info;
    dma2d_descriptor_t *d = s_rx_desc(h, 0);

    h->isr_next_strip = 0;
    d->dma2d_en   = JPEG_DMA2D_2D_ENABLE;
    d->mode       = DMA2D_DESCRIPTOR_BLOCK_RW_MODE_MULTIPLE;
    d->vb_size    = h->dma_vb;
    d->hb_length  = h->dma_hb;
    d->pbyte      = dma2d_desc_pixel_format_to_pbyte_value(h->engine->output_format);
    d->suc_eof    = JPEG_DMA2D_EOF_NOT_LAST;
    d->owner      = DMA2D_DESCRIPTOR_BUFFER_OWNER_DMA;
    d->va_size    = hi->process_v;
    d->ha_length  = hi->process_h;
    d->buffer     = out_buf;
    d->next       = NULL;
    d->err_eof    = 0;
    d->x = d->y   = 0;
    h->chain_tail = 0;

    esp_cache_msync(h->rxlinks, h->desc_byte_size, ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED | ESP_CACHE_MSYNC_FLAG_INVALIDATE);
    s_fill_tx_descriptor(h);
}

// =============================================================================
// 2D-DMA channel & event plumbing
// =============================================================================

// Descriptors are processed strictly in chain order, so we count strips with a
// simple in-order counter instead of trying to identify the descriptor from
// the (non-populated for RX_DONE) event data.
static IRAM_ATTR bool s_on_desc_done(dma2d_channel_handle_t chan, dma2d_event_data_t *evt, void *user_data)
{
    (void)chan; (void)evt;
    jpeg_enh_strip_decoder_handle_t h = (jpeg_enh_strip_decoder_handle_t)user_data;
    uint32_t idx = h->isr_next_strip++;
    if (h->whole_frame || idx >= h->frame.strip_count) return false;
    if (h->cfg.on_strip_done) {
        uint32_t y = idx * h->frame.strip_h;
        uint32_t padded = (idx + 1 == h->frame.strip_count) ? h->last_strip_rows : h->frame.strip_h;
        uint32_t valid = (y < h->frame.origin_h) ? (h->frame.origin_h - y) : 0;
        if (valid > padded) valid = padded;
        jpeg_enh_strip_event_t e = {
            .strip_idx   = idx,
            .buffer      = h->strip_bufs[idx % h->ring_count],
            .y_offset    = y,
            .rows        = valid,
            .padded_rows = padded,
        };
        return h->cfg.on_strip_done(&e, h->cfg.user_ctx);
    }
    return false;
}

static IRAM_ATTR bool s_on_recv_eof(dma2d_channel_handle_t chan, dma2d_event_data_t *evt, void *user_data)
{
    (void)chan; (void)evt;
    jpeg_enh_strip_decoder_handle_t h = (jpeg_enh_strip_decoder_handle_t)user_data;
    BaseType_t hp = pdFALSE;
    // Mirror the IDF JPEG decoder: post RX_EOF to the engine's event queue so
    // the waiting process() loop can unblock. Also release frame_done_sem so
    // the wait for the final strip callback is race-free.
    jpeg_dma2d_dec_evt_t e = { .dma_evt = JPEG_DMA2D_RX_EOF, .jpgd_status = 0 };
    xQueueSendFromISR(h->engine->evt_queue, &e, &hp);
    xSemaphoreGiveFromISR(h->frame_done_sem, &hp);
    return hp == pdTRUE;
}

static void s_dma_apply_jpeg_transfer_ability(jpeg_enh_strip_decoder_handle_t h,
                                              dma2d_channel_handle_t tx, dma2d_channel_handle_t rx)
{
    dma2d_transfer_ability_t tx_ab = {
        .data_burst_length = 128,
        .desc_burst_en = true,
        .mb_size = DMA2D_MACRO_BLOCK_SIZE_NONE,
    };
    dma2d_transfer_ability_t rx_ab = tx_ab;
    switch (h->engine->sample_method) {
    case JPEG_DOWN_SAMPLING_YUV444: rx_ab.mb_size = DMA2D_MACRO_BLOCK_SIZE_8_8;   break;
    case JPEG_DOWN_SAMPLING_YUV422: rx_ab.mb_size = DMA2D_MACRO_BLOCK_SIZE_8_16;  break;
    case JPEG_DOWN_SAMPLING_YUV420: rx_ab.mb_size = DMA2D_MACRO_BLOCK_SIZE_16_16; break;
    case JPEG_DOWN_SAMPLING_GRAY:   rx_ab.mb_size = DMA2D_MACRO_BLOCK_SIZE_8_8;   break;
    default: break;
    }
    dma2d_set_transfer_ability(tx, &tx_ab);
    dma2d_set_transfer_ability(rx, &rx_ab);
}

// Full-range YCbCr->RGB matrices (JPEG/JFIF convention, Y/Cb/Cr all in
// [0,255], output in [0,255]).
//
// The 2D-DMA CSC unit evaluates  256 * Q = A*Y + B*Cb + C*Cr + D  with field
// widths A[9:0]/B[10:0]/C[9:0]/D[17:0] (all signed two's complement, see
// in_color_param_h/m/l_ch0 in ESP32-P4 TRM section 8.4). The IDF defaults
// (DMA2D_COLOR_SPACE_CONV_PARAM_YUV2RGB_BT601/709) bake in the limited-range
// (Y_studio in [16,235]) 1.164*(Y-16) form, which under-saturates JPEG output
// because JPEG/MJPEG is full-range. We keep using DMA2D_CSC_RX_YUV*_TO_RGB*
// to drive input/output muxing + scramble setup, then overwrite the matrix.
//
// BT.601:  R = Y + 1.402(Cr-128);  G = Y - 0.344136(Cb-128) - 0.714136(Cr-128);
//          B = Y + 1.772(Cb-128)
// BT.709:  R = Y + 1.5748(Cr-128); G = Y - 0.18732(Cb-128) - 0.46812(Cr-128);
//          B = Y + 1.8556(Cb-128)
//
// Coefficients * 256 (rounded); offsets chosen so a neutral input
// (Y=0, Cb=128, Cr=128) maps to RGB=0 exactly.
static const int s_yuv2rgb_bt601_full_table[3][4] = {
    { 256,    0,   359,  -45952 },  // R: param_h
    { 256,  -88,  -183,   34688 },  // G: param_m
    { 256,  454,     0,  -58112 },  // B: param_l
};
static const int s_yuv2rgb_bt709_full_table[3][4] = {
    { 256,    0,   403,  -51584 },  // R: param_h
    { 256,  -48,  -120,   21504 },  // G: param_m
    { 256,  475,     0,  -60800 },  // B: param_l
};

static void s_dma_load_full_range_matrix(const int table[3][4])
{
    dma2d_dev_t *dev = DMA2D_LL_GET_HW(0);
    // Only RX channel 0 implements CSC (DMA2D_LL_RX_CHANNEL_SUPPORT_CSC_MASK = BIT0).
    volatile dma2d_color_param_group_chn_reg_t *grp = &dev->in_channel[0].in_color_param_group;
    grp->param_h.a = table[0][0];
    grp->param_h.b = table[0][1];
    grp->param_h.c = table[0][2];
    grp->param_h.d = table[0][3];
    grp->param_m.a = table[1][0];
    grp->param_m.b = table[1][1];
    grp->param_m.c = table[1][2];
    grp->param_m.d = table[1][3];
    grp->param_l.a = table[2][0];
    grp->param_l.b = table[2][1];
    grp->param_l.c = table[2][2];
    grp->param_l.d = table[2][3];
}

static void s_dma_apply_csc(jpeg_enh_strip_decoder_handle_t h, dma2d_channel_handle_t rx)
{
    dma2d_scramble_order_t post = DMA2D_SCRAMBLE_ORDER_BYTE2_1_0;
    dma2d_csc_rx_option_t opt = DMA2D_CSC_RX_NONE;
    bool yuv_to_rgb = false;

    if (h->engine->rgb_order == JPEG_DEC_RGB_ELEMENT_ORDER_RGB) {
        if (h->engine->output_format == JPEG_DECODE_OUT_FORMAT_RGB565) post = DMA2D_SCRAMBLE_ORDER_BYTE2_0_1;
        else if (h->engine->output_format == JPEG_DECODE_OUT_FORMAT_RGB888) post = DMA2D_SCRAMBLE_ORDER_BYTE0_1_2;
    }
    if (h->engine->output_format == JPEG_DECODE_OUT_FORMAT_RGB565) {
        opt = (h->engine->conv_std == JPEG_YUV_RGB_CONV_STD_BT601)
              ? DMA2D_CSC_RX_YUV420_TO_RGB565_601 : DMA2D_CSC_RX_YUV420_TO_RGB565_709;
        yuv_to_rgb = true;
    } else if (h->engine->output_format == JPEG_DECODE_OUT_FORMAT_RGB888) {
        opt = (h->engine->conv_std == JPEG_YUV_RGB_CONV_STD_BT601)
              ? DMA2D_CSC_RX_YUV420_TO_RGB888_601 : DMA2D_CSC_RX_YUV420_TO_RGB888_709;
        yuv_to_rgb = true;
    } else if (h->engine->output_format == JPEG_DECODE_OUT_FORMAT_YUV444) {
        if (h->engine->sample_method == JPEG_DOWN_SAMPLING_YUV422)      opt = DMA2D_CSC_RX_YUV422_TO_YUV444;
        else if (h->engine->sample_method == JPEG_DOWN_SAMPLING_YUV420) opt = DMA2D_CSC_RX_YUV420_TO_YUV444;
    }
    dma2d_csc_config_t cfg = { .post_scramble = post, .rx_csc_option = opt };
    dma2d_configure_color_space_conversion(rx, &cfg);
    // IDF just wrote the limited-range matrix; clobber it with the JFIF
    // full-range form when requested.
    if (yuv_to_rgb && h->cfg.decode.yuv_full_range) {
        s_dma_load_full_range_matrix(h->engine->conv_std == JPEG_YUV_RGB_CONV_STD_BT601
                                     ? s_yuv2rgb_bt601_full_table : s_yuv2rgb_bt709_full_table);
    }
}

static bool s_on_job_picked(uint32_t num_chans, const dma2d_trans_channel_info_t *chans, void *uc)
{
    jpeg_enh_strip_decoder_handle_t h = (jpeg_enh_strip_decoder_handle_t)uc;
    jpeg_hal_context_t *hal = &h->engine->codec_base->hal;
    assert(num_chans == 2);
    dma2d_channel_handle_t tx = NULL, rx = NULL;
    for (uint32_t i = 0; i < num_chans; i++) {
        if (chans[i].dir == DMA2D_CHANNEL_DIRECTION_TX) tx = chans[i].chan;
        else rx = chans[i].chan;
    }
    h->rx_chan = rx;
    h->engine->dma2d_tx_channel = tx;
    h->engine->dma2d_rx_channel = rx;

    dma2d_trigger_t trig = { .periph = DMA2D_TRIG_PERIPH_JPEG_DECODER,
                             .periph_sel_id = SOC_DMA2D_TRIG_PERIPH_JPEG_TX };
    dma2d_connect(tx, &trig);
    trig.periph_sel_id = SOC_DMA2D_TRIG_PERIPH_JPEG_RX;
    dma2d_connect(rx, &trig);

    s_dma_apply_jpeg_transfer_ability(h, tx, rx);
    s_dma_apply_csc(h, rx);

    // No owner_check: rely on the chain-extension backpressure — DMA only
    // ever sees descriptors whose buffers the consumer has released. No
    // on_desc_empty either: registering it would newly enable the DESC_EMPTY
    // interrupt, which the dma2d driver handles by freeing the channels (and
    // asserting the FSM is idle) — and whether that raw bit also raises at a
    // normal frame end is unverified. A mid-frame chain underrun therefore
    // surfaces as a decode timeout; s_wait_frame_done() diagnoses it from the
    // chain bookkeeping instead.
    dma2d_rx_event_callbacks_t cbs = {
        .on_recv_eof = s_on_recv_eof,
        .on_desc_done = s_on_desc_done,
        .on_desc_empty = NULL,
    };
    dma2d_register_rx_event_callbacks(rx, &cbs, h);

    dma2d_set_desc_addr(tx, (intptr_t)h->txlink);
    dma2d_set_desc_addr(rx, (intptr_t)h->rxlinks);
    dma2d_start(tx);
    dma2d_start(rx);
    jpeg_ll_process_start(hal->dev);
    return false;
}

// =============================================================================
// Frame-shared decode flow
// =============================================================================

// Parse + validate + fill h->frame for the picture at hand. Caller must hold
// the codec mutex.
static esp_err_t s_prepare_frame(jpeg_enh_strip_decoder_handle_t h,
                                 const uint8_t *bit_stream, uint32_t stream_size)
{
    esp_err_t err = s_parse_jpeg(h->engine, bit_stream, stream_size);
    if (err != ESP_OK) return err;
    err = s_apply_header_to_hw(h->engine);
    if (err != ESP_OK) return err;

    jpeg_dec_header_info_t *hi = h->engine->header_info;
    if (hi->process_h > DMA2D_LL_DESC_2D_FIELD_MAX || hi->process_v > DMA2D_LL_DESC_2D_FIELD_MAX) {
        ESP_LOGE(TAG, "picture %lux%lu exceeds the 2D-DMA descriptor limit",
                 (unsigned long)hi->process_h, (unsigned long)hi->process_v);
        return ESP_ERR_INVALID_SIZE;
    }

    s_compute_dma_blocks(h);

    h->frame.pic_w = hi->process_h;
    h->frame.pic_h = hi->process_v;
    h->frame.origin_w = hi->origin_h;
    h->frame.origin_h = hi->origin_v;
    h->frame.mcu_w = hi->mcux;
    h->frame.mcu_h = hi->mcuy;
    return ESP_OK;
}

// Wait for the JPEG event queue: either the JPEG hardware reports an error,
// the RX chain runs dry, or RX EOF lands. Caller must hold the codec mutex.
static esp_err_t s_wait_frame_done(jpeg_enh_strip_decoder_handle_t h)
{
    while (1) {
        jpeg_dma2d_dec_evt_t evt;
        BaseType_t r = xQueueReceive(h->engine->evt_queue, &evt, h->engine->timeout_tick);
        if (r != pdTRUE) {
            // Stall signature: the DMA consumed every linked descriptor and
            // is parked at the not-yet-spliced one, i.e. the consumer did not
            // release strips within the frame timeout.
            uint32_t done = h->isr_next_strip;
            if (!h->whole_frame && done < h->frame.strip_count && done == h->chain_tail + 1) {
                ESP_LOGE(TAG, "strip consumer stalled after %lu/%lu strips (no release_strip within timeout)",
                         (unsigned long)done, (unsigned long)h->frame.strip_count);
            } else {
                ESP_LOGE(TAG, "decode timeout (%lu/%lu strips)",
                         (unsigned long)done, (unsigned long)h->frame.strip_count);
            }
            return ESP_ERR_TIMEOUT;
        }
        if (evt.jpgd_status != 0) {
            ESP_LOGE(TAG, "decode jpgd_status=0x%lx", (unsigned long)evt.jpgd_status);
            return ESP_ERR_INVALID_STATE;
        }
        if (evt.dma_evt & JPEG_DMA2D_RX_EOF) return ESP_OK;
    }
}

static esp_err_t s_run_frame(jpeg_enh_strip_decoder_handle_t h)
{
    dma2d_trans_config_t trans = {
        .tx_channel_num = 1,
        .rx_channel_num = 1,
        .channel_flags = DMA2D_CHANNEL_FUNCTION_FLAG_RX_REORDER,
        .user_config = h,
        .on_job_picked = s_on_job_picked,
    };

    portENTER_CRITICAL(&h->spin);
    h->frame_active = true;
    portEXIT_CRITICAL(&h->spin);

    esp_err_t err = dma2d_enqueue(h->engine->dma2d_group_handle, &trans, h->engine->trans_desc);
    if (err == ESP_OK) {
        err = s_wait_frame_done(h);
    }

    // Stop release_strip() from touching the chain / DMA channel once the
    // transaction is over (or being force-ended). The 2D-DMA channels are
    // pooled and may be handed to another peripheral right after this.
    portENTER_CRITICAL(&h->spin);
    h->frame_active = false;
    h->rx_chan = NULL;
    portEXIT_CRITICAL(&h->spin);

    if (err != ESP_OK) {
        bool need_yield;
        dma2d_force_end(h->engine->trans_desc, &need_yield);
        return err;
    }

    // Confirm the final strip callback has fired; in the on_recv_eof path the
    // semaphore is signalled before the JPEG event is observed, so this take
    // should be immediate.
    xSemaphoreTake(h->frame_done_sem, h->engine->timeout_tick);
    return ESP_OK;
}

// =============================================================================
// Public API
// =============================================================================

static esp_err_t s_check_strip_buffer(void *buf, size_t size, size_t *usable)
{
    if (!(esp_ptr_external_ram(buf) || esp_ptr_dma_capable(buf))) {
        ESP_LOGE(TAG, "strip buffer %p is not 2D-DMA accessible", buf);
        return ESP_ERR_INVALID_ARG;
    }
    size_t align = 0;
    esp_cache_get_alignment(esp_ptr_external_ram(buf) ? MALLOC_CAP_SPIRAM : (MALLOC_CAP_INTERNAL | MALLOC_CAP_DMA), &align);
    if (align < STRIP_BUF_ALIGN) align = STRIP_BUF_ALIGN;
    if ((uintptr_t)buf % align != 0) {
        ESP_LOGE(TAG, "strip buffer %p must be %u-byte aligned", buf, (unsigned)align);
        return ESP_ERR_INVALID_ARG;
    }
    *usable = size - size % align;
    return ESP_OK;
}

esp_err_t jpeg_enh_strip_decoder_new(const jpeg_enh_strip_decoder_cfg_t *cfg,
                                     jpeg_enh_strip_decoder_handle_t *out_handle)
{
    if (!cfg || !out_handle) return ESP_ERR_INVALID_ARG;
    if (!cfg->strip_bufs[0] && cfg->strip_bufs[1]) return ESP_ERR_INVALID_ARG;
    uint32_t ring_count = cfg->strip_bufs[0] ? (cfg->strip_bufs[1] ? 2 : 1) : 0;
    if (ring_count && (!cfg->on_strip_done || cfg->strip_buf_size == 0)) return ESP_ERR_INVALID_ARG;

    size_t usable = SIZE_MAX;
    for (uint32_t i = 0; i < ring_count; i++) {
        size_t u;
        esp_err_t err = s_check_strip_buffer(cfg->strip_bufs[i], cfg->strip_buf_size, &u);
        if (err != ESP_OK) return err;
        if (u < usable) usable = u;
    }

    jpeg_enh_strip_decoder_handle_t h = heap_caps_calloc(1, sizeof(*h), MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    if (!h) return ESP_ERR_NO_MEM;
    h->cfg = *cfg;
    h->spin = (portMUX_TYPE)portMUX_INITIALIZER_UNLOCKED;
    h->ring_count = ring_count;
    for (uint32_t i = 0; i < ring_count; i++) h->strip_bufs[i] = cfg->strip_bufs[i];
    h->strip_buf_size = ring_count ? usable : 0;

    jpeg_decode_engine_cfg_t ecfg = {
        .intr_priority = cfg->intr_priority,
        .timeout_ms = cfg->timeout_ms ? cfg->timeout_ms : DEFAULT_TIMEOUT_MS,
    };
    esp_err_t err = jpeg_new_decoder_engine(&ecfg, &h->engine);
    if (err != ESP_OK) goto fail;
    h->engine->output_format = cfg->decode.output_format;
    h->engine->rgb_order = cfg->decode.rgb_order;
    h->engine->conv_std = cfg->decode.conv_std;

    uint32_t cache_line = cache_hal_get_cache_line_size(CACHE_LL_LEVEL_EXT_MEM, CACHE_TYPE_DATA);
    h->desc_byte_size = s_align_up(sizeof(dma2d_descriptor_t), cache_line);
    h->rxlinks = heap_caps_aligned_calloc(cache_line, RX_DESC_POOL, h->desc_byte_size,
                                          MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    h->txlink  = heap_caps_aligned_calloc(cache_line, 1, h->desc_byte_size,
                                          MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    if (!h->rxlinks || !h->txlink) { err = ESP_ERR_NO_MEM; goto fail; }

    // Stale cache lines over the buffers (from the caller's allocation or
    // earlier CPU use) would otherwise be written back over DMA output,
    // e.g. by PPA's input sync. Internal RAM is cached on ESP32-P4 too.
    for (uint32_t i = 0; i < ring_count; i++) {
        esp_cache_msync(h->strip_bufs[i], h->strip_buf_size,
                        ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_INVALIDATE);
    }

    h->frame_done_sem = xSemaphoreCreateBinary();
    if (!h->frame_done_sem) { err = ESP_ERR_NO_MEM; goto fail; }

    *out_handle = h;
    return ESP_OK;
fail:
    jpeg_enh_strip_decoder_del(h);
    return err;
}

esp_err_t jpeg_enh_strip_decoder_del(jpeg_enh_strip_decoder_handle_t h)
{
    if (!h) return ESP_OK;
    if (h->rxlinks) free(h->rxlinks);
    if (h->txlink)  free(h->txlink);
    if (h->frame_done_sem) vSemaphoreDelete(h->frame_done_sem);
    if (h->engine) jpeg_del_decoder_engine(h->engine);
    free(h);
    return ESP_OK;
}

esp_err_t jpeg_enh_strip_decoder_process(jpeg_enh_strip_decoder_handle_t h,
                                         const uint8_t *bit_stream, uint32_t stream_size,
                                         jpeg_enh_frame_info_t *info)
{
    if (!h || !bit_stream || !stream_size) return ESP_ERR_INVALID_ARG;
    if (!h->ring_count) return ESP_ERR_INVALID_STATE;

    esp_err_t err = ESP_OK;
#if CONFIG_PM_ENABLE
    if (h->engine->codec_base->pm_lock) esp_pm_lock_acquire(h->engine->codec_base->pm_lock);
#endif
    xSemaphoreTake(h->engine->codec_base->codec_mutex, portMAX_DELAY);
    xQueueReset(h->engine->evt_queue);
    // Drain any stale semaphore from a previous error path
    xSemaphoreTake(h->frame_done_sem, 0);
    h->whole_frame = false;

    err = s_prepare_frame(h, bit_stream, stream_size);
    if (err != ESP_OK) goto out;

    // MCU heights (8/16) divide STRIP_ROW_ALIGN, so strips also land on MCU rows.
    {
        uint64_t row_bits = (uint64_t)h->frame.pic_w * s_bits_per_pixel(h->cfg.decode.output_format);
        uint32_t rows = (uint32_t)((uint64_t)h->strip_buf_size * 8 / row_bits);
        uint32_t strip_h = rows - rows % STRIP_ROW_ALIGN;
        uint32_t cap = s_align_up((h->frame.pic_h + h->ring_count - 1) / h->ring_count, STRIP_ROW_ALIGN);
        if (strip_h > cap) strip_h = cap;
        if (strip_h == 0) {
            ESP_LOGE(TAG, "%u-byte strip buffers hold fewer than %u rows at width %lu",
                     (unsigned)h->strip_buf_size, STRIP_ROW_ALIGN, (unsigned long)h->frame.pic_w);
            err = ESP_ERR_INVALID_SIZE;
            goto out;
        }
        h->frame.strip_h = strip_h;
        h->frame.strip_count = (h->frame.pic_h + strip_h - 1) / strip_h;
        h->last_strip_rows = h->frame.pic_h - (h->frame.strip_count - 1) * strip_h;
    }

    if (info) *info = h->frame;
    if (h->cfg.on_frame_start) {
        err = h->cfg.on_frame_start(&h->frame, h->cfg.user_ctx);
        if (err != ESP_OK) goto out;
    }

    s_init_strip_descriptors(h);

    // Sync input from cache to memory so DMA reads the right bytes
    esp_cache_msync((void *)h->engine->header_info->buffer_offset,
                    h->engine->header_info->buffer_left,
                    ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED);
    // Strip buffers were purged from cache in new() and CPU consumers go
    // through sync_strip_for_cpu(), so nothing to do per frame.

    err = s_run_frame(h);

out:
    xSemaphoreGive(h->engine->codec_base->codec_mutex);
#if CONFIG_PM_ENABLE
    if (h->engine->codec_base->pm_lock) esp_pm_lock_release(h->engine->codec_base->pm_lock);
#endif
    return err;
}

esp_err_t jpeg_enh_decoder_process(jpeg_enh_strip_decoder_handle_t h,
                                   const uint8_t *bit_stream, uint32_t stream_size,
                                   void *out_buf, size_t out_buf_size,
                                   jpeg_enh_frame_info_t *info)
{
    if (!h || !bit_stream || !stream_size || !out_buf) return ESP_ERR_INVALID_ARG;

    esp_err_t err = ESP_OK;
    bool buf_external = esp_ptr_external_ram(out_buf);
    size_t cache_sync_len = 0;

#if CONFIG_PM_ENABLE
    if (h->engine->codec_base->pm_lock) esp_pm_lock_acquire(h->engine->codec_base->pm_lock);
#endif
    xSemaphoreTake(h->engine->codec_base->codec_mutex, portMAX_DELAY);
    xQueueReset(h->engine->evt_queue);
    xSemaphoreTake(h->frame_done_sem, 0);
    h->whole_frame = true;

    err = s_prepare_frame(h, bit_stream, stream_size);
    if (err != ESP_OK) goto out;
    h->frame.strip_h = h->frame.pic_h;
    h->frame.strip_count = 1;
    h->last_strip_rows = h->frame.pic_h;

    {
        size_t need = (size_t)h->frame.pic_h * h->frame.pic_w * s_bits_per_pixel(h->cfg.decode.output_format) / 8;
        cache_sync_len = need;
        if (buf_external) {
            // PSRAM output goes through the cache: the buffer must be
            // cache-line aligned and big enough that whole-line maintenance
            // stays inside it (jpeg_alloc_decoder_mem guarantees both).
            size_t ext_align = 0;
            esp_cache_get_alignment(MALLOC_CAP_SPIRAM, &ext_align);
            cache_sync_len = s_align_up(need, ext_align);
            if ((uintptr_t)out_buf % ext_align != 0) {
                ESP_LOGE(TAG, "output buffer must be %u-byte aligned (use jpeg_alloc_decoder_mem)", (unsigned)ext_align);
                err = ESP_ERR_INVALID_ARG;
                goto out;
            }
        }
        if (out_buf_size < cache_sync_len) {
            ESP_LOGE(TAG, "output buffer too small: need %u (cache-aligned), have %u",
                     (unsigned)cache_sync_len, (unsigned)out_buf_size);
            err = ESP_ERR_INVALID_SIZE;
            goto out;
        }
    }

    if (info) *info = h->frame;
    if (h->cfg.on_frame_start) {
        err = h->cfg.on_frame_start(&h->frame, h->cfg.user_ctx);
        if (err != ESP_OK) goto out;
    }

    s_init_whole_frame_descriptor(h, out_buf);

    esp_cache_msync((void *)h->engine->header_info->buffer_offset,
                    h->engine->header_info->buffer_left,
                    ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED);
    if (buf_external) {
        // Flush + drop any cached lines of the output region so nothing can
        // be written back over the DMA output mid-decode.
        esp_cache_msync(out_buf, cache_sync_len,
                        ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_INVALIDATE);
    }

    err = s_run_frame(h);

    if (err == ESP_OK && buf_external) {
        // Make the DMA-written pixels visible to the CPU.
        esp_cache_msync(out_buf, cache_sync_len, ESP_CACHE_MSYNC_FLAG_DIR_M2C);
    }

out:
    xSemaphoreGive(h->engine->codec_base->codec_mutex);
#if CONFIG_PM_ENABLE
    if (h->engine->codec_base->pm_lock) esp_pm_lock_release(h->engine->codec_base->pm_lock);
#endif
    return err;
}

// IRAM: called from the Layer 2 PPA-done ISR (see jpeg_ppa_pipeline.c).
IRAM_ATTR esp_err_t jpeg_enh_strip_decoder_release_strip(jpeg_enh_strip_decoder_handle_t h, uint32_t strip_idx)
{
    if (!h || !h->ring_count) return ESP_ERR_INVALID_ARG;
    // The buffer strip_idx used is next needed by strip strip_idx + ring_count;
    // fill that strip's descriptor and splice it in now that the buffer is free.
    uint32_t new_tail = strip_idx + h->ring_count;
    if (new_tail >= h->frame.strip_count) {
        return ESP_OK;
    }
    dma2d_descriptor_t *prev = s_strip_desc(h, new_tail - 1);
    dma2d_descriptor_t *next = s_strip_desc(h, new_tail);

    s_fill_strip_descriptor(h, new_tail);
    esp_cache_msync(next, h->desc_byte_size, ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED | ESP_CACHE_MSYNC_FLAG_INVALIDATE);

    portENTER_CRITICAL_SAFE(&h->spin);
    // Once the frame is over (EOF or error/force-end) the chain must not be
    // touched and the pooled DMA channel may belong to someone else. On clean
    // frames this gate never closes early: EOF implies every splice already
    // happened (the last strip can only complete after all earlier release
    // calls extended the chain to reach it).
    if (!h->frame_active || !h->rx_chan) {
        portEXIT_CRITICAL_SAFE(&h->spin);
        return ESP_OK;
    }
    // prev used to be the tail (with .next = NULL). Link it onward and kick
    // the DMA so it picks the extension up.
    prev->next = next;
    esp_cache_msync(prev, h->desc_byte_size, ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED | ESP_CACHE_MSYNC_FLAG_INVALIDATE);
    h->chain_tail = new_tail;
    dma2d_append(h->rx_chan);
    portEXIT_CRITICAL_SAFE(&h->spin);
    return ESP_OK;
}

esp_err_t jpeg_enh_strip_decoder_sync_strip_for_cpu(jpeg_enh_strip_decoder_handle_t h,
                                                    const jpeg_enh_strip_event_t *evt)
{
    if (!h || !evt || !evt->buffer) return ESP_ERR_INVALID_ARG;
    esp_err_t err = esp_cache_msync(evt->buffer, h->strip_buf_size, ESP_CACHE_MSYNC_FLAG_DIR_M2C);
    return err == ESP_ERR_NOT_SUPPORTED ? ESP_OK : err;
}

uint32_t jpeg_enh_strip_decoder_strips_delivered(jpeg_enh_strip_decoder_handle_t h)
{
    if (!h) return 0;
    uint32_t n = h->isr_next_strip;
    return n > h->frame.strip_count ? h->frame.strip_count : n;
}
