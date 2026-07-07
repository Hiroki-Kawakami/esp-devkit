// Host (simulator) shim for hal/ppa_types.h. Per the idf_compat rule the PPA is
// an Espressif-defined API, so the host mirrors the IDF type header verbatim
// (pure enums, no I/O) — the color-mode ids are Four Character Codes (see
// hal/color_types.h) so they match the device, keeping app code source-portable.
// Kept in sync with ESP-IDF v6.0.2 hal/ppa_types.h — v6.0 moved the color-mode
// ids from COLOR_TYPE_ID to FourCC and made the burst length a raw byte count
// (no longer a dma2d_data_burst_length_t alias, so hal/dma2d_types.h is gone).
#pragma once

#include <stdint.h>
#include "hal/color_types.h"

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Enumeration of engines in PPA modules
 */
typedef enum {
    PPA_ENGINE_TYPE_SRM,            /*!< PPA Scaling-Rotating-Mirroring (SRM) engine */
    PPA_ENGINE_TYPE_BLEND,          /*!< PPA Blending engine, used to perform blend or fill */
} ppa_engine_type_t;

/**
 * @brief Enumeration of PPA Scaling-Rotating-Mirroring available rotation angle (counterclockwise)
 */
typedef enum {
    PPA_SRM_ROTATION_ANGLE_0,        /*!< Picture does no rotation */
    PPA_SRM_ROTATION_ANGLE_90,       /*!< Picture rotates 90 degrees CCW */
    PPA_SRM_ROTATION_ANGLE_180,      /*!< Picture rotates 180 degrees CCW */
    PPA_SRM_ROTATION_ANGLE_270,      /*!< Picture rotates 270 degrees CCW */
} ppa_srm_rotation_angle_t;

/**
 * @brief Enumeration of PPA Scaling-Rotating-Mirroring available color mode
 */
typedef enum {
    PPA_SRM_COLOR_MODE_ARGB8888 = ESP_COLOR_FOURCC_BGRA32,      /*!< ARGB8888 */
    PPA_SRM_COLOR_MODE_RGB888 = ESP_COLOR_FOURCC_BGR24,         /*!< RGB888 */
    PPA_SRM_COLOR_MODE_RGB565 = ESP_COLOR_FOURCC_RGB16,         /*!< RGB565 */
    PPA_SRM_COLOR_MODE_YUV420 = ESP_COLOR_FOURCC_OUYY_EVYY,     /*!< YUV420 */
    PPA_SRM_COLOR_MODE_YUV444 = ESP_COLOR_FOURCC_YUV,           /*!< YUV444 (limited range, input only) */
    PPA_SRM_COLOR_MODE_YUV422_UYVY = ESP_COLOR_FOURCC_UYVY,     /*!< YUV422 */
    PPA_SRM_COLOR_MODE_YUV422_VYUY = ESP_COLOR_FOURCC_VYUY,     /*!< YUV422, input only */
    PPA_SRM_COLOR_MODE_YUV422_YUYV = ESP_COLOR_FOURCC_YUYV,     /*!< YUV422, input only */
    PPA_SRM_COLOR_MODE_YUV422_YVYU = ESP_COLOR_FOURCC_YVYU,     /*!< YUV422, input only */
    PPA_SRM_COLOR_MODE_GRAY8 = ESP_COLOR_FOURCC_GREY,           /*!< GRAY8 */
} ppa_srm_color_mode_t;

/**
 * @brief Enumeration of PPA blend available color mode
 */
typedef enum {
    PPA_BLEND_COLOR_MODE_ARGB8888 = ESP_COLOR_FOURCC_BGRA32,    /*!< ARGB8888 */
    PPA_BLEND_COLOR_MODE_RGB888 = ESP_COLOR_FOURCC_BGR24,       /*!< RGB888 */
    PPA_BLEND_COLOR_MODE_RGB565 = ESP_COLOR_FOURCC_RGB16,       /*!< RGB565 */
    PPA_BLEND_COLOR_MODE_A8 = ESP_COLOR_FOURCC_ALPHA8,          /*!< A8, foreground only */
    PPA_BLEND_COLOR_MODE_A4 = ESP_COLOR_FOURCC_ALPHA4,          /*!< A4, foreground only */
    PPA_BLEND_COLOR_MODE_YUV420 = ESP_COLOR_FOURCC_OUYY_EVYY,   /*!< YUV420, background/output only */
    PPA_BLEND_COLOR_MODE_YUV422_UYVY = ESP_COLOR_FOURCC_UYVY,   /*!< YUV422, background/output only */
    PPA_BLEND_COLOR_MODE_YUV422_VYUY = ESP_COLOR_FOURCC_VYUY,   /*!< YUV422, background only */
    PPA_BLEND_COLOR_MODE_YUV422_YUYV = ESP_COLOR_FOURCC_YUYV,   /*!< YUV422, background only */
    PPA_BLEND_COLOR_MODE_YUV422_YVYU = ESP_COLOR_FOURCC_YVYU,   /*!< YUV422, background only */
    PPA_BLEND_COLOR_MODE_GRAY8 = ESP_COLOR_FOURCC_GREY,         /*!< GRAY8, background/output only */
} ppa_blend_color_mode_t;

/**
 * @brief Enumeration of PPA fill available color mode
 */
typedef enum {
    PPA_FILL_COLOR_MODE_ARGB8888 = ESP_COLOR_FOURCC_BGRA32,     /*!< ARGB8888 */
    PPA_FILL_COLOR_MODE_RGB888 = ESP_COLOR_FOURCC_BGR24,        /*!< RGB888 */
    PPA_FILL_COLOR_MODE_RGB565 = ESP_COLOR_FOURCC_RGB16,        /*!< RGB565 */
    PPA_FILL_COLOR_MODE_YUV422_UYVY = ESP_COLOR_FOURCC_UYVY,    /*!< YUV422 (UYVY pack order) */
    PPA_FILL_COLOR_MODE_GRAY8 = ESP_COLOR_FOURCC_GREY,          /*!< GRAY8 */
} ppa_fill_color_mode_t;

/**
 * @brief Enumeration of PPA alpha compositing update mode
 */
typedef enum {
    PPA_ALPHA_NO_CHANGE = 0,  /*!< Do not replace alpha value (A' = A). If no alpha info, 255 is used. */
    PPA_ALPHA_FIX_VALUE,      /*!< Replace the alpha value with a new, fixed alpha value (A' = val) */
    PPA_ALPHA_SCALE,          /*!< Scale the alpha value (A' = (A * val) >> 8). If no alpha info, A = 255. */
    PPA_ALPHA_INVERT,         /*!< Invert the alpha value (A' = 255 - A). If no alpha info, A' = 0. */
} ppa_alpha_update_mode_t;

/**
 * @brief Enumeration of PPA supported color conversion standard between RGB and YUV
 */
typedef enum {
    PPA_COLOR_CONV_STD_RGB_YUV_BT601 = COLOR_CONV_STD_RGB_YUV_BT601,      /*!< BT.601 */
    PPA_COLOR_CONV_STD_RGB_YUV_BT709 = COLOR_CONV_STD_RGB_YUV_BT709,      /*!< BT.709 */
} ppa_color_conv_std_rgb_yuv_t;

/**
 * @brief Enumeration of PPA supported color range
 */
typedef enum {
    PPA_COLOR_RANGE_LIMIT = COLOR_RANGE_LIMIT,      /*!< Limited color range */
    PPA_COLOR_RANGE_FULL = COLOR_RANGE_FULL,        /*!< Full color range */
} ppa_color_range_t;

/**
 * @brief Enumeration of PPA supported data burst length
 */
typedef enum {
    PPA_DATA_BURST_LENGTH_8 = 8,        /*!< Data burst length: 8 bytes */
    PPA_DATA_BURST_LENGTH_16 = 16,      /*!< Data burst length: 16 bytes */
    PPA_DATA_BURST_LENGTH_32 = 32,      /*!< Data burst length: 32 bytes */
    PPA_DATA_BURST_LENGTH_64 = 64,      /*!< Data burst length: 64 bytes */
    PPA_DATA_BURST_LENGTH_128 = 128,    /*!< Data burst length: 128 bytes */
} ppa_data_burst_length_t;

#ifdef __cplusplus
}
#endif
