/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * jpeg_decode_enhanced — internal PPA SRM submission path for Layer 2.
 *
 * Frame-scoped replacement for ppa_do_scale_rotate_mirror(): everything that
 * is constant over a frame is validated and precomputed once in begin_frame(),
 * and each strip is then submitted straight to 2D-DMA from ISR context.
 * Other PPA SRM clients are held off between begin_frame() and end_frame().
 */

#pragma once
#include <stdint.h>
#include <stdbool.h>
#include "esp_err.h"
#include "driver/ppa.h"

typedef struct ppa_srm_fast_s *ppa_srm_fast_handle_t;

/**
 * @brief Strip-done callback. Device: 2D-DMA ISR context. Host: called from
 *        inside ppa_srm_fast_submit() before it returns.
 * @return Whether a higher-priority task was woken.
 */
typedef bool (*ppa_srm_fast_done_cb_t)(void *ctx);

esp_err_t ppa_srm_fast_new(ppa_srm_fast_done_cb_t cb, void *ctx, ppa_srm_fast_handle_t *out);
void ppa_srm_fast_del(ppa_srm_fast_handle_t h);

/**
 * @brief Validate and latch the frame-constant operation, then take the SRM
 *        engine. `tmpl->in` provides buffer-independent input fields (pic_w,
 *        block_offset_x, block_w, srm_cm, yuv_*); `tmpl->out` the whole output.
 *        Blocks until other SRM clients have drained.
 *
 * Input buffers passed to submit() are not cache-synced: they must only ever be
 * written by DMA.
 */
esp_err_t ppa_srm_fast_begin_frame(ppa_srm_fast_handle_t h, const ppa_srm_oper_config_t *tmpl);

/**
 * @brief Submit one strip. ISR-safe. On ESP_OK the done callback fires exactly
 *        once; on error it does not fire.
 */
esp_err_t ppa_srm_fast_submit(ppa_srm_fast_handle_t h, void *in_buf, uint32_t in_pic_h,
                              uint32_t in_offset_y, uint32_t in_block_h,
                              uint32_t out_offset_x, uint32_t out_offset_y);

/**
 * @brief Force-end a submission whose done callback never came.
 * @return ESP_ERR_INVALID_STATE if it is still queued in 2D-DMA and could not
 *         be cancelled; the handle must not be used for another frame then.
 */
esp_err_t ppa_srm_fast_abort(ppa_srm_fast_handle_t h);

/**
 * @brief Release the SRM engine to other clients. Pair with a successful
 *        begin_frame(), after the last done callback (or abort).
 */
void ppa_srm_fast_end_frame(ppa_srm_fast_handle_t h);
