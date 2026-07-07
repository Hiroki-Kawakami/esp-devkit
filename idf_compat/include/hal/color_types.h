// Host (simulator) shim for hal/color_types.h. Per the idf_compat rule this is
// an Espressif-defined API, so the host mirrors the IDF header *itself* (pure
// type definitions, no I/O) rather than re-abstracting it. Pulled in transitively
// by driver/ppa.h via hal/ppa_types.h — the PPA op configs reference the
// color_pixel_* pixel structs directly and the color-mode ids are derived from
// the Four Character Codes below, so the values must match IDF for source
// portability. Kept in sync with ESP-IDF v6.0.2 hal/color_types.h — v6.0 replaced
// the old COLOR_TYPE_ID(color_space, pixel_format) scheme with these FourCC ids.
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Four Character Code type definition
 */
typedef uint32_t esp_color_fourcc_t;

#define ESP_COLOR_FOURCC(a, b, c, d) ((uint32_t)(a) | ((uint32_t)(b) << 8) | ((uint32_t)(c) << 16) | ((uint32_t)(d) << 24))

#define ESP_COLOR_FOURCC_BGRA32         ESP_COLOR_FOURCC('B', 'A', '2', '4') /* 32 bpp BGRA-8-8-8-8 */
#define ESP_COLOR_FOURCC_BGR24          ESP_COLOR_FOURCC('B', 'G', 'R', '3') /* 24 bpp BGR-8-8-8 */
#define ESP_COLOR_FOURCC_RGB24          ESP_COLOR_FOURCC('R', 'G', 'B', '3') /* 24 bpp RGB-8-8-8 */
#define ESP_COLOR_FOURCC_RGB16          ESP_COLOR_FOURCC('R', 'G', 'B', 'L') /* 16 bpp RGB-5-6-5, little endian */
#define ESP_COLOR_FOURCC_RGB16_BE       ESP_COLOR_FOURCC('R', 'G', 'B', 'E') /* 16 bpp RGB-5-6-5, big endian */
#define ESP_COLOR_FOURCC_GREY           ESP_COLOR_FOURCC('G', 'R', 'E', 'Y') /* 8 bpp Greyscale */
#define ESP_COLOR_FOURCC_ALPHA4         ESP_COLOR_FOURCC('A', 'L', 'P', '4') /* 4 bpp, alpha-only */
#define ESP_COLOR_FOURCC_ALPHA8         ESP_COLOR_FOURCC('A', 'L', 'P', '8') /* 8 bpp, alpha-only */
#define ESP_COLOR_FOURCC_YUV            ESP_COLOR_FOURCC('V', '3', '0', '8') /* 24 bpp, Y-U-V 4:4:4 */
#define ESP_COLOR_FOURCC_YUYV           ESP_COLOR_FOURCC('Y', 'U', 'Y', 'V') /* 16 bpp, YUYV 4:2:2 */
#define ESP_COLOR_FOURCC_YVYU           ESP_COLOR_FOURCC('Y', 'V', 'Y', 'U') /* 16 bpp, YVYU 4:2:2 */
#define ESP_COLOR_FOURCC_UYVY           ESP_COLOR_FOURCC('U', 'Y', 'V', 'Y') /* 16 bpp, UYVY 4:2:2 */
#define ESP_COLOR_FOURCC_VYUY           ESP_COLOR_FOURCC('V', 'Y', 'U', 'Y') /* 16 bpp, VYUY 4:2:2 */
#define ESP_COLOR_FOURCC_OUYY_EVYY      ESP_COLOR_FOURCC('O', 'U', 'E', 'V') /* 12 bpp, Espressif Y-U-V 4:2:0 */
#define ESP_COLOR_FOURCC_RAW8           ESP_COLOR_FOURCC('R', 'A', 'W', '8') /* 8 bpp, raw8 */
#define ESP_COLOR_FOURCC_RAW10          ESP_COLOR_FOURCC('R', 'A', 'W', 'A') /* 10 bpp, raw10 */
#define ESP_COLOR_FOURCC_RAW12          ESP_COLOR_FOURCC('R', 'A', 'W', 'C') /* 12 bpp, raw12 */
#define ESP_COLOR_FOURCC_RAW16          ESP_COLOR_FOURCC('R', 'A', 'W', 'G') /* 16 bpp, raw16 */

/**
 * @brief Color range
 */
typedef enum {
    COLOR_RANGE_LIMIT, /*!< Limited color range, 16 is the darkest black and 235 is the brightest white */
    COLOR_RANGE_FULL,  /*!< Full color range, 0 is the darkest black and 255 is the brightest white */
} color_range_t;

/**
 * @brief The standard used for conversion between RGB and YUV
 */
typedef enum {
    COLOR_CONV_STD_RGB_YUV_BT601, /*!< YUV<->RGB conversion standard: BT.601 */
    COLOR_CONV_STD_RGB_YUV_BT709, /*!< YUV<->RGB conversion standard: BT.709 */
} color_conv_std_rgb_yuv_t;

/**
 * @brief RGB element order
 */
typedef enum {
    COLOR_RGB_ELEMENT_ORDER_RGB, /*!< RGB element order: RGB */
    COLOR_RGB_ELEMENT_ORDER_BGR, /*!< RGB element order: BGR */
} color_rgb_element_order_t;

/**
 * @brief Data structure for ARGB8888 pixel unit
 */
typedef union {
    struct {
        uint32_t b: 8;      /*!< B component [0, 255] */
        uint32_t g: 8;      /*!< G component [0, 255] */
        uint32_t r: 8;      /*!< R component [0, 255] */
        uint32_t a: 8;      /*!< A component [0, 255] */
    };
    uint32_t val;           /*!< 32-bit ARGB8888 value */
} color_pixel_argb8888_data_t;

/**
 * @brief Data structure for RGB888 pixel unit
 */
typedef union {
    struct {
        uint8_t b;          /*!< B component [0, 255] */
        uint8_t g;          /*!< G component [0, 255] */
        uint8_t r;          /*!< R component [0, 255] */
    };
    uint32_t val;           /*!< 32-bit RGB888 value */
} color_pixel_rgb888_data_t;

/**
 * @brief Data structure for RGB565 pixel unit
 */
typedef union {
    struct {
        uint16_t b: 5;      /*!< B component [0, 31] */
        uint16_t g: 6;      /*!< G component [0, 63] */
        uint16_t r: 5;      /*!< R component [0, 31] */
    };
    uint16_t val;           /*!< 16-bit RGB565 value */
} color_pixel_rgb565_data_t;

/**
 * @brief Data structure for GRAY8 pixel unit
 */
typedef union {
    struct {
        uint8_t gray;      /*!< Gray component [0, 255] */
    };
    uint8_t val;           /*!< 8-bit GRAY8 value */
} color_pixel_gray8_data_t;

/**
 * @brief Data structure for YUV macroblock unit
 */
typedef struct {
    uint8_t y;      /*!< Y component [0, 255] */
    uint8_t u;      /*!< U component [0, 255] */
    uint8_t v;      /*!< V component [0, 255] */
} color_macroblock_yuv_data_t;

#ifdef __cplusplus
}
#endif
