/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * jpeg_decode_enhanced — Layer 2: strip-pipelined JPEG decode + PPA SRM.
 *
 * Decodes a JPEG into caller-owned strip buffers (Layer 1) and pushes each
 * strip through PPA SRM into a destination frame buffer as soon as it lands,
 * in parallel with the decode of subsequent strips. Resources (strip buffers,
 * JPEG engine, PPA client) are fixed at creation; everything
 * about the transform — rotation, scale, mirror, crop, output placement —
 * can change per frame.
 *
 * Geometry note: each strip goes through PPA as an independent block, so
 * every interior strip boundary must scale to a whole output pixel row or
 * the strips would overlap/gap. PPA quantizes scale factors to multiples of
 * 1/16 (and 1/8 for YUV420 output); jpeg_ppa_pipeline_process() validates
 * "(boundary row × quantized scale_y) is an integer" per frame and rejects
 * the transform otherwise. Strips are always 16-row multiples, so this only
 * fails when in_crop.y × scale_y is not a whole row.
 */

#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include "esp_err.h"
#include "driver/ppa.h"
#include "jpeg_decode_enhanced.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct jpeg_ppa_pipeline_s *jpeg_ppa_pipeline_handle_t;

/**
 * @brief Pipeline resources configuration (fixed at creation).
 */
typedef struct {
    void *strip_bufs[2];                  /*!< Caller-owned strip buffers; see jpeg_enh_strip_decoder_cfg_t.
                                               [1] NULL = no pipelining (decode and PPA alternate). */
    size_t strip_buf_size;                /*!< Byte size of each strip buffer */
    ppa_srm_color_mode_t strip_color_mode;/*!< Intermediate strip pixel format. Determines the JPEG
                                               decode output and the PPA SRM input color mode.
                                               RGB565 needs 2/3 of the RGB888 memory; YUV420 needs a
                                               YUV420 source JPEG (no transcode in hardware). */
    jpeg_dec_rgb_element_order_t rgb_order;/*!< Element order of the decoded strips (RGB modes) */
    jpeg_yuv_rgb_conv_std_t conv_std;     /*!< BT601 or BT709 for the decode-side YUV->RGB CSC */
    bool yuv_full_range;                  /*!< Full-range (JFIF) decode matrix; see jpeg_enh_decode_cfg_t */
    uint32_t timeout_ms;                  /*!< Per-frame decode timeout, and the wait for the last PPA
                                               strips after decode (0 = 200 ms) */
} jpeg_ppa_pipeline_cfg_t;

typedef struct {
    uint32_t x, y, w, h;
} jpeg_ppa_rect_t;

/**
 * @brief Per-frame transform. All fields may change on every process() call.
 *        Zero-initialized = render the full image 1:1 at the output origin
 *        (scale 0 is treated as 1.0).
 */
typedef struct {
    ppa_srm_rotation_angle_t rotation;    /*!< PPA_SRM_ROTATION_ANGLE_{0,90,180,270} */
    float scale_x;                        /*!< Horizontal scale (input x axis); 0 = 1.0 */
    float scale_y;                        /*!< Vertical scale (input y axis); 0 = 1.0 */
    bool mirror_x;                        /*!< Mirror the rendered image horizontally (output space) */
    bool mirror_y;                        /*!< Mirror the rendered image vertically (output space) */
    bool rgb_swap;                        /*!< PPA input RGB<->BGR swap */
    bool byte_swap;                       /*!< PPA input byte swap (RGB565/ARGB8888 strips only) */
    jpeg_ppa_rect_t in_crop;              /*!< Input-space crop; w==0 or h==0 = full (valid) image.
                                               Pixels outside origin_w/origin_h are always cropped. */
    uint32_t out_offset_x;                /*!< Top-left of the rendered rect in the output picture */
    uint32_t out_offset_y;
    jpeg_ppa_rect_t out_clip;
} jpeg_ppa_transform_t;

/**
 * @brief Per-frame output description.
 */
typedef struct {
    void *buffer;                         /*!< Output frame buffer (PPA alignment rules apply) */
    size_t buffer_size;                   /*!< 0 = pic_w * pic_h * bytes_per_pixel */
    uint32_t pic_w;                       /*!< Output picture width (pixels) */
    uint32_t pic_h;                       /*!< Output picture height (pixels) */
    ppa_srm_color_mode_t color_mode;      /*!< Output pixel format */
    ppa_color_range_t yuv_range;          /*!< Output YUV range (YUV output modes only) */
    ppa_color_conv_std_rgb_yuv_t yuv_std; /*!< Output RGB->YUV standard (YUV output modes only) */
} jpeg_ppa_output_t;

/**
 * @brief Create the pipeline: strip decoder (Layer 1) and PPA SRM client.
 *        The strip buffers stay owned by the caller.
 */
esp_err_t jpeg_ppa_pipeline_new(const jpeg_ppa_pipeline_cfg_t *cfg,
                                jpeg_ppa_pipeline_handle_t *out_handle);

/**
 * @brief Tear down the pipeline and release all resources.
 */
esp_err_t jpeg_ppa_pipeline_del(jpeg_ppa_pipeline_handle_t handle);

/**
 * @brief The Layer 1 decoder the pipeline drives. Owned by the pipeline and
 *        valid until del(). Use it for jpeg_enh_decoder_process() when a frame
 *        can land in its destination without PPA; never call
 *        jpeg_enh_strip_decoder_process() on it, and never concurrently with
 *        jpeg_ppa_pipeline_process().
 */
jpeg_enh_strip_decoder_handle_t jpeg_ppa_pipeline_get_decoder(jpeg_ppa_pipeline_handle_t handle);

/**
 * @brief Decode one JPEG and render it into `out->buffer` with `transform`.
 *        Blocks until every strip has been pushed through PPA.
 *
 * Pixels of the output picture outside the rendered rect are left untouched.
 * The PPA SRM engine is held for the whole frame: other PPA SRM clients
 * (ppa_do_scale_rotate_mirror) wait until this returns.
 *
 * @param transform NULL = identity (full image, no rotation/scale, origin 0,0)
 * @param info      Optional; receives the frame geometry
 */
esp_err_t jpeg_ppa_pipeline_process(jpeg_ppa_pipeline_handle_t handle,
                                    const void *jpeg_data, size_t jpeg_size,
                                    const jpeg_ppa_output_t *out,
                                    const jpeg_ppa_transform_t *transform,
                                    jpeg_enh_frame_info_t *info);

#ifdef __cplusplus
}
#endif
