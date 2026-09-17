/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "display_manager.hpp"

#include <cmath>
#include <cstring>
#include <new>

#include "bsp.h"
#include "esp_heap_caps.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

#ifdef ESP_PLATFORM
#include "soc/soc_caps.h"
#if SOC_PPA_SUPPORTED
#define DISPLAY_MANAGER_USE_PPA 1
#endif
#else
#define DISPLAY_MANAGER_USE_PPA 1
#endif

#ifdef DISPLAY_MANAGER_USE_PPA
#include "driver/ppa.h"
#endif

namespace {

constexpr uint8_t kMaxOperations = 6;
constexpr int kPrimaryTouchId = 0;

enum class DisplayRenderPath : uint8_t {
    Direct,
    Partial,
    Surface,
};

bool swaps_axes(bsp_rotation_t rotation) {
    return rotation == BSP_ROTATION_90 || rotation == BSP_ROTATION_270;
}

lv_color_format_t lv_color_format(bsp_pixel_format_t format) {
    switch (format) {
        case BSP_PIXEL_FORMAT_L8:     return LV_COLOR_FORMAT_L8;
        case BSP_PIXEL_FORMAT_RGB888: return LV_COLOR_FORMAT_RGB888;
        default:                      return LV_COLOR_FORMAT_RGB565;
    }
}

bool bsp_pixel_format(lv_color_format_t format, bsp_pixel_format_t *out) {
    switch (format) {
        case LV_COLOR_FORMAT_L8:
            *out = BSP_PIXEL_FORMAT_L8;
            return true;
        case LV_COLOR_FORMAT_RGB565:
            *out = BSP_PIXEL_FORMAT_RGB565;
            return true;
        case LV_COLOR_FORMAT_RGB888:
            *out = BSP_PIXEL_FORMAT_RGB888;
            return true;
        default:
            return false;
    }
}

bsp_rect_t full_panel_rect(bsp_size_t size) {
    return {{0, 0}, size};
}

bsp_rect_t bsp_rect(const lv_area_t &area) {
    return {
        {area.x1, area.y1},
        {area.x2 - area.x1 + 1, area.y2 - area.y1 + 1},
    };
}

bool same_rect(bsp_rect_t lhs, bsp_rect_t rhs) {
    return lhs.origin.x == rhs.origin.x &&
        lhs.origin.y == rhs.origin.y &&
        lhs.size.width == rhs.size.width &&
        lhs.size.height == rhs.size.height;
}

bool valid_rect(bsp_rect_t rect, bsp_size_t bounds) {
    return rect.origin.x >= 0 && rect.origin.y >= 0 &&
        rect.size.width > 0 && rect.size.height > 0 &&
        rect.origin.x + rect.size.width <= bounds.width &&
        rect.origin.y + rect.size.height <= bounds.height;
}

} // namespace

struct DisplayManagerContext;

struct DisplayFlushContext {
    lv_display_t *display;
    lv_area_t area;
    uint8_t *pixels;
    bool last;
    esp_err_t result = ESP_OK;
};

struct DisplayManagerContext {
    using Operation = void (*)(DisplayManagerContext &, DisplayFlushContext &);

    Operation operations[kMaxOperations] = {};
    uint8_t operation_count = 0;

    DisplayRenderPath render_path = DisplayRenderPath::Surface;
    uint8_t bytes_per_pixel = 0;
    uint8_t visible : 1 = true;
    uint8_t epd_enabled : 1 = false;
    uint8_t next_epd_mode_valid : 1 = false;
    uint8_t dirty_valid : 1 = false;
    uint8_t input_down : 1 = false;
    uint8_t input_armed : 1 = false;
    uint8_t input_pending : 1 = false;
    uint8_t owns_buffers : 1 = false;

    bsp_size_t panel_size = {};
    bsp_size_t logical_size = {};
    bsp_rect_t output_area = {};
    bsp_rotation_t rotation = BSP_ROTATION_0;
    bsp_pixel_format_t color_format = BSP_PIXEL_FORMAT_RGB565;
    bsp_pixel_format_t panel_format = BSP_PIXEL_FORMAT_RGB565;
    float scale_x = 1.0f;
    float scale_y = 1.0f;

    void *ppa_srm = nullptr;
    bsp_epd_mode_t default_epd_mode = BSP_EPD_MODE_FAST;
    bsp_epd_mode_t next_epd_mode = BSP_EPD_MODE_NONE;
    lv_area_t dirty = {};

    size_t buffer_bytes = 0;
    lv_display_t *display = nullptr;
    lv_indev_t *indev = nullptr;
    lv_point_t input_point = {};
    void *buffer0 = nullptr;
    void *buffer1 = nullptr;

    void append(Operation operation) {
        if (operation_count < kMaxOperations) {
            operations[operation_count++] = operation;
        }
    }

    void execute(DisplayFlushContext &context) {
        for (uint8_t i = 0; i < operation_count; ++i) {
            operations[i](*this, context);
        }
    }
};

namespace {

void free_owned_buffers(DisplayManagerContext &context) {
    if (!context.owns_buffers) return;
    heap_caps_free(context.buffer0);
    if (context.buffer1) heap_caps_free(context.buffer1);
}

bool buffer_aligned(void *buffer, bsp_pixel_format_t format) {
    return !buffer || lv_draw_buf_align(buffer, lv_color_format(format)) == buffer;
}

bool point_inside(const DisplayManagerContext &display,
                  const bsp_touch_point_t &point) {
    return display.visible &&
        point.x >= display.output_area.origin.x &&
        point.y >= display.output_area.origin.y &&
        point.x < display.output_area.origin.x + display.output_area.size.width &&
        point.y < display.output_area.origin.y + display.output_area.size.height;
}

lv_point_t map_touch_point(const DisplayManagerContext &display,
                           const bsp_touch_point_t &point) {
    int x = point.x - display.output_area.origin.x;
    int y = point.y - display.output_area.origin.y;
    int output_width = display.output_area.size.width;
    int output_height = display.output_area.size.height;
    int logical_width = display.logical_size.width;
    int logical_height = display.logical_size.height;

    if (x < 0) x = 0;
    if (y < 0) y = 0;
    if (x >= output_width) x = output_width - 1;
    if (y >= output_height) y = output_height - 1;

    lv_point_t result;
    switch (display.rotation) {
        case BSP_ROTATION_90:
            result.x = logical_width - 1 -
                (int)((int64_t)y * logical_width / output_height);
            result.y = (int)((int64_t)x * logical_height / output_width);
            break;
        case BSP_ROTATION_180:
            result.x = logical_width - 1 -
                (int)((int64_t)x * logical_width / output_width);
            result.y = logical_height - 1 -
                (int)((int64_t)y * logical_height / output_height);
            break;
        case BSP_ROTATION_270:
            result.x = (int)((int64_t)y * logical_width / output_height);
            result.y = logical_height - 1 -
                (int)((int64_t)x * logical_height / output_width);
            break;
        default:
            result.x = (int)((int64_t)x * logical_width / output_width);
            result.y = (int)((int64_t)y * logical_height / output_height);
            break;
    }
    return result;
}

void clear_input_state(DisplayManagerContext &display) {
    display.input_down = false;
    display.input_armed = false;
    display.input_pending = false;
}

bsp_size_t derive_logical_size(bsp_size_t output_size, bsp_rotation_t rotation) {
    return swaps_axes(rotation)
        ? (bsp_size_t){output_size.height, output_size.width}
        : output_size;
}

void update_scale(DisplayManagerContext &context) {
    if (swaps_axes(context.rotation)) {
        context.scale_x = (float)context.output_area.size.height /
            context.logical_size.width;
        context.scale_y = (float)context.output_area.size.width /
            context.logical_size.height;
    } else {
        context.scale_x = (float)context.output_area.size.width /
            context.logical_size.width;
        context.scale_y = (float)context.output_area.size.height /
            context.logical_size.height;
    }
}

bool scale_is_one(const DisplayManagerContext &context) {
    return std::fabs(context.scale_x - 1.0f) < 0.0001f &&
        std::fabs(context.scale_y - 1.0f) < 0.0001f;
}

lv_area_t map_area(const DisplayManagerContext &display, const lv_area_t &source) {
    lv_area_t result = source;
    int width = display.logical_size.width;
    int height = display.logical_size.height;

    switch (display.rotation) {
        case BSP_ROTATION_90:
            result.x1 = source.y1;
            result.x2 = source.y2;
            result.y1 = width - source.x2 - 1;
            result.y2 = width - source.x1 - 1;
            break;
        case BSP_ROTATION_180:
            result.x1 = width - source.x2 - 1;
            result.x2 = width - source.x1 - 1;
            result.y1 = height - source.y2 - 1;
            result.y2 = height - source.y1 - 1;
            break;
        case BSP_ROTATION_270:
            result.x1 = height - source.y2 - 1;
            result.x2 = height - source.y1 - 1;
            result.y1 = source.x1;
            result.y2 = source.x2;
            break;
        default:
            break;
    }

    result.x1 += display.output_area.origin.x;
    result.x2 += display.output_area.origin.x;
    result.y1 += display.output_area.origin.y;
    result.y2 += display.output_area.origin.y;
    return result;
}

void composite_cpu(const DisplayManagerContext &display, void *framebuffer) {
    const uint8_t *source = static_cast<const uint8_t *>(display.buffer0);
    auto *output = static_cast<uint8_t *>(framebuffer);
    int source_width = display.logical_size.width;
    int source_height = display.logical_size.height;
    int output_width = display.output_area.size.width;
    int output_height = display.output_area.size.height;
    size_t bytes = display.bytes_per_pixel;

    for (int y = 0; y < output_height; ++y) {
        for (int x = 0; x < output_width; ++x) {
            int sx;
            int sy;
            switch (display.rotation) {
                case BSP_ROTATION_90:
                    sx = source_width - 1 - y * source_width / output_height;
                    sy = x * source_height / output_width;
                    break;
                case BSP_ROTATION_180:
                    sx = source_width - 1 - x * source_width / output_width;
                    sy = source_height - 1 - y * source_height / output_height;
                    break;
                case BSP_ROTATION_270:
                    sx = y * source_width / output_height;
                    sy = source_height - 1 - x * source_height / output_width;
                    break;
                default:
                    sx = x * source_width / output_width;
                    sy = y * source_height / output_height;
                    break;
            }

            size_t source_offset = ((size_t)sy * source_width + sx) * bytes;
            int dx = display.output_area.origin.x + x;
            int dy = display.output_area.origin.y + y;
            size_t output_offset = ((size_t)dy * display.panel_size.width + dx) * bytes;
            std::memcpy(output + output_offset, source + source_offset, bytes);
        }
    }
}

#ifdef DISPLAY_MANAGER_USE_PPA
bool ppa_format(bsp_pixel_format_t format, ppa_srm_color_mode_t *out) {
    switch (format) {
        case BSP_PIXEL_FORMAT_RGB565:
            *out = PPA_SRM_COLOR_MODE_RGB565;
            return true;
        case BSP_PIXEL_FORMAT_RGB888:
            *out = PPA_SRM_COLOR_MODE_RGB888;
            return true;
        default:
            return false;
    }
}

ppa_srm_rotation_angle_t ppa_rotation(bsp_rotation_t rotation) {
    switch (rotation) {
        case BSP_ROTATION_90:  return PPA_SRM_ROTATION_ANGLE_90;
        case BSP_ROTATION_180: return PPA_SRM_ROTATION_ANGLE_180;
        case BSP_ROTATION_270: return PPA_SRM_ROTATION_ANGLE_270;
        default:               return PPA_SRM_ROTATION_ANGLE_0;
    }
}
#endif

bool convertible(bsp_pixel_format_t from, bsp_pixel_format_t to) {
    if (from == to) return true;
#ifdef DISPLAY_MANAGER_USE_PPA
    ppa_srm_color_mode_t mode;
    return ppa_format(from, &mode) && ppa_format(to, &mode);
#else
    return false;
#endif
}

esp_err_t composite_surface(DisplayManagerContext &display, void *framebuffer) {
    if (display.render_path != DisplayRenderPath::Surface ||
        !display.buffer0 || !framebuffer) {
        return ESP_ERR_INVALID_STATE;
    }

#ifdef DISPLAY_MANAGER_USE_PPA
    ppa_srm_color_mode_t in_format;
    ppa_srm_color_mode_t out_format;
    if (ppa_format(display.color_format, &in_format) &&
        ppa_format(display.panel_format, &out_format)) {
        auto client = static_cast<ppa_client_handle_t>(display.ppa_srm);
        if (!client) {
            ppa_client_config_t config = {};
            config.oper_type = PPA_OPERATION_SRM;
            esp_err_t err = ppa_register_client(&config, &client);
            if (err != ESP_OK) return err;
            display.ppa_srm = client;
        }

        ppa_srm_oper_config_t operation = {};
        operation.in.buffer = display.buffer0;
        operation.in.pic_w = display.logical_size.width;
        operation.in.pic_h = display.logical_size.height;
        operation.in.block_w = display.logical_size.width;
        operation.in.block_h = display.logical_size.height;
        operation.in.srm_cm = in_format;
        operation.out.buffer = framebuffer;
        operation.out.buffer_size = (uint32_t)display.panel_size.width *
            display.panel_size.height *
            bsp_pixel_format_bytes(display.panel_format);
        operation.out.pic_w = display.panel_size.width;
        operation.out.pic_h = display.panel_size.height;
        operation.out.block_offset_x = display.output_area.origin.x;
        operation.out.block_offset_y = display.output_area.origin.y;
        operation.out.srm_cm = out_format;
        operation.rotation_angle = ppa_rotation(display.rotation);
        operation.scale_x = display.scale_x;
        operation.scale_y = display.scale_y;
        operation.mode = PPA_TRANS_MODE_BLOCKING;
        return ppa_do_scale_rotate_mirror(client, &operation);
    }
#endif

    if (display.color_format != display.panel_format) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    composite_cpu(display, framebuffer);
    return ESP_OK;
}

void map_flush_area(DisplayManagerContext &display, DisplayFlushContext &flush) {
    flush.area = map_area(display, flush.area);
}

void draw_bitmap(DisplayManagerContext &display, DisplayFlushContext &flush) {
    if (flush.result != ESP_OK || !display.visible) return;
    bsp_display_draw_bitmap(bsp_rect(flush.area), flush.pixels, display.rotation);
}

void draw_bitmap_async(DisplayManagerContext &display, DisplayFlushContext &flush) {
    if (flush.result != ESP_OK || !display.visible) return;
    bsp_display_draw_bitmap_async(bsp_rect(flush.area), flush.pixels,
                                  display.rotation);
}

void accumulate_dirty(DisplayManagerContext &display, DisplayFlushContext &flush) {
    if (flush.result != ESP_OK || !display.visible) return;
    if (!display.dirty_valid) {
        display.dirty = flush.area;
        display.dirty_valid = true;
        return;
    }

    if (flush.area.x1 < display.dirty.x1) display.dirty.x1 = flush.area.x1;
    if (flush.area.y1 < display.dirty.y1) display.dirty.y1 = flush.area.y1;
    if (flush.area.x2 > display.dirty.x2) display.dirty.x2 = flush.area.x2;
    if (flush.area.y2 > display.dirty.y2) display.dirty.y2 = flush.area.y2;
}

void refresh_epd(DisplayManagerContext &display, DisplayFlushContext &flush) {
    if (!flush.last) return;

    bsp_epd_mode_t mode = display.next_epd_mode_valid
        ? display.next_epd_mode
        : display.default_epd_mode;
    display.next_epd_mode_valid = false;

    if (mode != BSP_EPD_MODE_NONE && display.dirty_valid) {
        bsp_display_refresh(bsp_rect(display.dirty), mode);
    }
    display.dirty_valid = false;
}

void flush_framebuffer(DisplayManagerContext &display, DisplayFlushContext &flush) {
    if (display.render_path != DisplayRenderPath::Direct && !flush.last) return;
    if (flush.result != ESP_OK || !display.visible) return;

    int framebuffer_index = display.render_path == DisplayRenderPath::Direct &&
        flush.pixels == display.buffer1 ? 1 : 0;
    bsp_display_flush(framebuffer_index);
}

void composite_immediate(DisplayManagerContext &display,
                         DisplayFlushContext &flush) {
    if (!flush.last || !display.visible) return;
    void *framebuffer = bsp_display_get_frame_buffer(0);
    flush.result = composite_surface(display, framebuffer);
}

void flush_ready(DisplayManagerContext &, DisplayFlushContext &flush) {
    lv_display_flush_ready(flush.display);
}

} // namespace

esp_err_t DisplayManager::create_display(const DisplayManagerConfig &config,
                                         lv_display_t **out_display) {
    if (!out_display) return ESP_ERR_INVALID_ARG;
    *out_display = nullptr;

    uint8_t display_slot = kMaxDisplays;
    for (uint8_t i = 0; i < kMaxDisplays; ++i) {
        if (!displays_[i]) {
            display_slot = i;
            break;
        }
    }
    if (display_slot == kMaxDisplays) return ESP_ERR_NO_MEM;

    bsp_size_t panel_size = bsp_display_get_size();
    if (panel_size.width <= 0 || panel_size.height <= 0) {
        return ESP_ERR_INVALID_STATE;
    }

    bsp_rect_t output_area = config.viewport.output_area;
    if (output_area.size.width <= 0 || output_area.size.height <= 0) {
        output_area = full_panel_rect(panel_size);
    }
    if (!valid_rect(output_area, panel_size)) return ESP_ERR_INVALID_ARG;

    bsp_size_t logical_size = config.viewport.logical_size;
    if (logical_size.width <= 0 || logical_size.height <= 0) {
        logical_size = derive_logical_size(output_area.size,
                                           config.viewport.rotation);
    }
    if (logical_size.width <= 0 || logical_size.height <= 0) {
        return ESP_ERR_INVALID_ARG;
    }

    bsp_pixel_format_t panel_format = bsp_display_get_pixel_format();
    bsp_pixel_format_t color_format = panel_format;
    if (config.color_format != LV_COLOR_FORMAT_UNKNOWN &&
        !bsp_pixel_format(config.color_format, &color_format)) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    bool converted = color_format != panel_format;

    uint32_t caps = bsp_display_get_caps();
    bool has_framebuffer = caps & BSP_DISPLAY_CAP_FRAMEBUFFER;
    bool is_epd = caps & BSP_DISPLAY_CAP_EPD_REFRESH;
    bool immediate = config.present_mode == DisplayPresentMode::Immediate;
    bool full_output = same_rect(output_area, full_panel_rect(panel_size));
    bool identity = full_output &&
        logical_size.width == panel_size.width &&
        logical_size.height == panel_size.height &&
        config.viewport.rotation == BSP_ROTATION_0;

    DisplayRenderPath render_path;
    switch (config.render_mode) {
        case DisplayRenderMode::Direct:
            if (!immediate || !has_framebuffer || !identity || is_epd) {
                return ESP_ERR_NOT_SUPPORTED;
            }
            render_path = DisplayRenderPath::Direct;
            break;
        case DisplayRenderMode::Partial:
            if (!immediate) return ESP_ERR_INVALID_ARG;
            render_path = DisplayRenderPath::Partial;
            break;
        case DisplayRenderMode::Surface:
            render_path = DisplayRenderPath::Surface;
            break;
        default:
            if (converted) {
                render_path = DisplayRenderPath::Surface;
            } else if (immediate && has_framebuffer && identity && !is_epd) {
                render_path = DisplayRenderPath::Direct;
            } else if (immediate && (!has_framebuffer || is_epd)) {
                render_path = DisplayRenderPath::Partial;
            } else {
                render_path = DisplayRenderPath::Surface;
            }
            break;
    }
    if (render_path == DisplayRenderPath::Surface && !has_framebuffer) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    if (converted && (render_path != DisplayRenderPath::Surface ||
                      !convertible(color_format, panel_format))) {
        return ESP_ERR_NOT_SUPPORTED;
    }

    auto *context = new (std::nothrow) DisplayManagerContext;
    if (!context) return ESP_ERR_NO_MEM;

    context->render_path = render_path;
    context->panel_size = panel_size;
    context->logical_size = logical_size;
    context->output_area = output_area;
    context->rotation = config.viewport.rotation;
    context->color_format = color_format;
    context->panel_format = panel_format;
    context->bytes_per_pixel = bsp_pixel_format_bytes(color_format);
    context->epd_enabled = is_epd;
    update_scale(*context);

    if (context->bytes_per_pixel == 0) {
        delete context;
        return ESP_ERR_NOT_SUPPORTED;
    }

    void *const *buffers = config.buffer.buffers;
    bool external = buffers[0];
    size_t long_edge_bytes = (size_t)(logical_size.width > logical_size.height
        ? logical_size.width
        : logical_size.height) * context->bytes_per_pixel;
    size_t surface_bytes = (size_t)logical_size.width * logical_size.height *
        context->bytes_per_pixel;
    bool buffers_valid = !buffers[1] || external;
    if (external) {
        switch (render_path) {
            case DisplayRenderPath::Direct:
                buffers_valid = false;
                break;
            case DisplayRenderPath::Partial:
                buffers_valid = buffers_valid &&
                    config.buffer.buffer_size >= long_edge_bytes;
                break;
            case DisplayRenderPath::Surface:
                buffers_valid = buffers_valid && !buffers[1] &&
                    config.buffer.buffer_size >= surface_bytes;
                break;
        }
        buffers_valid = buffers_valid &&
            buffer_aligned(buffers[0], color_format) &&
            buffer_aligned(buffers[1], color_format);
    }
    if (!buffers_valid) {
        delete context;
        return ESP_ERR_INVALID_ARG;
    }

    size_t buffer_bytes = 0;
    lv_display_render_mode_t render_mode;
    if (render_path == DisplayRenderPath::Direct) {
        context->buffer0 = bsp_display_get_frame_buffer(0);
        if (!context->buffer0) {
            delete context;
            return ESP_ERR_INVALID_STATE;
        }
        context->buffer1 = bsp_display_get_frame_buffer(1);
        buffer_bytes = (size_t)logical_size.width * logical_size.height *
            context->bytes_per_pixel;
        render_mode = LV_DISPLAY_RENDER_MODE_DIRECT;
        context->append(flush_framebuffer);
    } else if (render_path == DisplayRenderPath::Partial) {
        if (!scale_is_one(*context)) {
            delete context;
            return ESP_ERR_NOT_SUPPORTED;
        }

        if (external) {
            buffer_bytes = config.buffer.buffer_size;
            context->buffer0 = buffers[0];
            context->buffer1 = buffers[1];
        } else {
            int lines = config.buffer.lines > 0
                ? config.buffer.lines
                : logical_size.height / 4;
            if (lines < 1) lines = 1;
            int count = config.buffer.count > 0
                ? config.buffer.count
                : (is_epd ? 1 : 2);
            if (count < 1) count = 1;
            if (count > 2) count = 2;

            buffer_bytes = (size_t)logical_size.width * lines *
                context->bytes_per_pixel;
            /* Rotation re-slices the same budget by the new stride, so one row
             * of the long edge is the floor. */
            if (buffer_bytes < long_edge_bytes) buffer_bytes = long_edge_bytes;
            buffer_bytes = (buffer_bytes + 63) & ~(size_t)63;
            uint32_t memory_caps = has_framebuffer ? MALLOC_CAP_SPIRAM
                                                   : MALLOC_CAP_DEFAULT;
            context->owns_buffers = true;
            context->buffer0 = heap_caps_aligned_alloc(64, buffer_bytes, memory_caps);
            if (count == 2 && context->buffer0) {
                context->buffer1 = heap_caps_aligned_alloc(64, buffer_bytes, memory_caps);
            }
            if (!context->buffer0 || (count == 2 && !context->buffer1)) {
                if (context->buffer0) heap_caps_free(context->buffer0);
                if (context->buffer1) heap_caps_free(context->buffer1);
                delete context;
                return ESP_ERR_NO_MEM;
            }
        }
        render_mode = LV_DISPLAY_RENDER_MODE_PARTIAL;
        context->append(map_flush_area);
        /* LVGL renders the next chunk into the other buffer once flush_ready
         * returns, so only a second buffer lets the blit run behind it. */
        context->append(context->buffer1 ? draw_bitmap_async : draw_bitmap);
        if (is_epd) {
            context->append(accumulate_dirty);
            context->append(refresh_epd);
        } else if (has_framebuffer) {
            context->append(flush_framebuffer);
        }
    } else {
        if (external) {
            buffer_bytes = config.buffer.buffer_size;
            context->buffer0 = buffers[0];
        } else {
            buffer_bytes = surface_bytes;
            context->owns_buffers = true;
            context->buffer0 = heap_caps_aligned_alloc(64, buffer_bytes,
                                                       MALLOC_CAP_SPIRAM);
            if (!context->buffer0) {
                delete context;
                return ESP_ERR_NO_MEM;
            }
        }
        std::memset(context->buffer0, 0, surface_bytes);
        render_mode = LV_DISPLAY_RENDER_MODE_DIRECT;
        if (immediate) {
            context->append(composite_immediate);
            context->append(flush_framebuffer);
        }
    }
    context->append(flush_ready);
    context->buffer_bytes = buffer_bytes;

    context->display = lv_display_create(logical_size.width, logical_size.height);
    if (!context->display) {
        free_owned_buffers(*context);
        delete context;
        return ESP_ERR_NO_MEM;
    }

    lv_display_set_color_format(context->display,
                                lv_color_format(color_format));
    lv_display_set_buffers(context->display, context->buffer0, context->buffer1,
                           buffer_bytes, render_mode);
    lv_display_set_user_data(context->display, context);
    lv_display_set_flush_cb(context->display, flush_cb);
    if (config.make_default) lv_display_set_default(context->display);

    if (!touch_mutex_) {
        touch_mutex_ = xSemaphoreCreateMutex();
        if (!touch_mutex_) {
            lv_display_delete(context->display);
            free_owned_buffers(*context);
            delete context;
            return ESP_ERR_NO_MEM;
        }
        bsp_touch_set_event_cb(touch_event_cb, this);
    }

    context->indev = lv_indev_create();
    if (!context->indev) {
        lv_display_delete(context->display);
        free_owned_buffers(*context);
        delete context;
        return ESP_ERR_NO_MEM;
    }
    lv_indev_set_type(context->indev, LV_INDEV_TYPE_POINTER);
    lv_indev_set_user_data(context->indev, context);
    lv_indev_set_read_cb(context->indev, indev_read_cb);
    lv_indev_set_display(context->indev, context->display);
    lv_indev_set_mode(context->indev, LV_INDEV_MODE_EVENT);

    auto touch_mutex = static_cast<SemaphoreHandle_t>(touch_mutex_);
    xSemaphoreTake(touch_mutex, portMAX_DELAY);
    displays_[display_slot] = context;
    xSemaphoreGive(touch_mutex);

    if (is_epd) bsp_display_set_epd_mode(BSP_EPD_MODE_NONE);

    *out_display = context->display;
    return ESP_OK;
}

esp_err_t DisplayManager::delete_display(lv_display_t *display) {
    DisplayManagerContext *context = context_for(display);
    if (!context) return ESP_ERR_INVALID_ARG;

    auto touch_mutex = static_cast<SemaphoreHandle_t>(touch_mutex_);
    if (touch_mutex) xSemaphoreTake(touch_mutex, portMAX_DELAY);
    uint8_t slot = kMaxDisplays;
    for (uint8_t i = 0; i < kMaxDisplays; ++i) {
        if (displays_[i] == context) {
            slot = i;
            break;
        }
    }
    if (slot < kMaxDisplays) {
        displays_[slot] = nullptr;
        release_input_locked(*context);
    }
    if (touch_mutex) xSemaphoreGive(touch_mutex);
    if (slot == kMaxDisplays) return ESP_ERR_INVALID_ARG;

    bsp_display_wait_draw();
    /* lv_display_delete only detaches indevs. */
    lv_indev_delete(context->indev);
    lv_display_delete(context->display);
#ifdef DISPLAY_MANAGER_USE_PPA
    if (context->ppa_srm) {
        ppa_unregister_client(static_cast<ppa_client_handle_t>(context->ppa_srm));
    }
#endif
    free_owned_buffers(*context);
    delete context;
    return ESP_OK;
}

esp_err_t DisplayManager::set_rotation(lv_display_t *display,
                                       bsp_rotation_t rotation) {
    DisplayManagerContext *context = context_for(display);
    if (!context) return ESP_ERR_INVALID_ARG;

    if (context->render_path == DisplayRenderPath::Direct &&
        rotation != BSP_ROTATION_0) {
        return ESP_ERR_NOT_SUPPORTED;
    }

    auto touch_mutex = static_cast<SemaphoreHandle_t>(touch_mutex_);
    if (touch_mutex) xSemaphoreTake(touch_mutex, portMAX_DELAY);

    bsp_rotation_t previous = context->rotation;
    bsp_size_t previous_size = context->logical_size;
    /* A scaling viewport keeps the resolution it was given; an unscaled one
     * follows the rotation. */
    bool unscaled = scale_is_one(*context);
    context->rotation = rotation;
    if (unscaled) {
        context->logical_size = derive_logical_size(context->output_area.size,
                                                    rotation);
    }
    update_scale(*context);
    if (context->render_path == DisplayRenderPath::Partial &&
        !scale_is_one(*context)) {
        context->rotation = previous;
        context->logical_size = previous_size;
        update_scale(*context);
        if (touch_mutex) xSemaphoreGive(touch_mutex);
        return ESP_ERR_NOT_SUPPORTED;
    }
    bsp_size_t logical_size = context->logical_size;
    if (touch_mutex) xSemaphoreGive(touch_mutex);

    if (rotation == previous) return ESP_OK;

    if (logical_size.width != previous_size.width ||
        logical_size.height != previous_size.height) {
        lv_display_set_resolution(display, logical_size.width,
                                  logical_size.height);
        /* The draw buffers carry the resolution's stride, so they have to be
         * handed back after the swap. */
        lv_display_set_buffers(display, context->buffer0, context->buffer1,
                               context->buffer_bytes,
                               context->render_path == DisplayRenderPath::Partial
                                   ? LV_DISPLAY_RENDER_MODE_PARTIAL
                                   : LV_DISPLAY_RENDER_MODE_DIRECT);
    } else {
        lv_obj_t *screen = lv_display_get_screen_active(display);
        if (screen) lv_obj_invalidate(screen);
    }
    return ESP_OK;
}

esp_err_t DisplayManager::set_epd_mode(lv_display_t *display,
                                       bsp_epd_mode_t mode, bool once) {
    DisplayManagerContext *context = context_for(display);
    if (!context || !context->epd_enabled) return ESP_ERR_NOT_SUPPORTED;

    if (once) {
        context->next_epd_mode = mode;
        context->next_epd_mode_valid = true;
    } else {
        context->default_epd_mode = mode;
    }
    return ESP_OK;
}

bool DisplayManager::release_input_locked(DisplayManagerContext &context) {
    bool reset_input = context.input_down || context.input_armed ||
        context.input_pending;
    if (active_touch_display_ == &context) {
        active_touch_display_ = nullptr;
        outside_touch_active_ = false;
        reset_input = true;
    }
    clear_input_state(context);
    return reset_input;
}

esp_err_t DisplayManager::set_visible(lv_display_t *display, bool visible) {
    DisplayManagerContext *context = context_for(display);
    if (!context) return ESP_ERR_INVALID_ARG;
    if (context->visible == visible) return ESP_OK;

    /* Areas invalidated before this point would still render on the next
     * refresh, so they are drawn out while the display is still shown. */
    if (!visible) lv_refr_now(display);

    bool reset_input = false;
    auto touch_mutex = static_cast<SemaphoreHandle_t>(touch_mutex_);
    if (touch_mutex) xSemaphoreTake(touch_mutex, portMAX_DELAY);
    context->visible = visible;
    if (!visible) reset_input = release_input_locked(*context);
    if (touch_mutex) xSemaphoreGive(touch_mutex);

    if (reset_input) lv_indev_reset(context->indev, nullptr);
    lv_display_enable_invalidation(display, visible);
    if (visible) {
        lv_obj_t *screen = lv_display_get_screen_active(display);
        if (screen) lv_obj_invalidate(screen);
    }
    return ESP_OK;
}

esp_err_t DisplayManager::set_outside_touch_callback(TouchCallback callback,
                                                      void *arg) {
    auto touch_mutex = static_cast<SemaphoreHandle_t>(touch_mutex_);
    if (!touch_mutex) return ESP_ERR_INVALID_STATE;
    xSemaphoreTake(touch_mutex, portMAX_DELAY);
    outside_touch_callback_ = callback;
    outside_touch_arg_ = arg;
    xSemaphoreGive(touch_mutex);
    return ESP_OK;
}

esp_err_t DisplayManager::compose(lv_display_t *display,
                                  int framebuffer_index) {
    DisplayManagerContext *context = context_for(display);
    if (!context) return ESP_ERR_INVALID_ARG;

    if (!context->visible) return ESP_OK;
    if (context->render_path == DisplayRenderPath::Partial) return ESP_OK;
    if (context->render_path != DisplayRenderPath::Surface) {
        return ESP_ERR_NOT_SUPPORTED;
    }

    void *framebuffer = bsp_display_get_frame_buffer(framebuffer_index);
    if (!framebuffer) return ESP_ERR_INVALID_ARG;
    return composite_surface(*context, framebuffer);
}

esp_err_t DisplayManager::present(int framebuffer_index) {
    if (!(bsp_display_get_caps() & BSP_DISPLAY_CAP_FRAMEBUFFER)) return ESP_OK;

    void *framebuffer = bsp_display_get_frame_buffer(framebuffer_index);
    if (!framebuffer) return ESP_ERR_INVALID_ARG;

    bsp_display_flush(framebuffer_index);
    return ESP_OK;
}

void DisplayManager::flush_cb(lv_display_t *display, const lv_area_t *area,
                              uint8_t *pixels) {
    auto *context = static_cast<DisplayManagerContext *>(
        lv_display_get_user_data(display));
    DisplayFlushContext flush{
        display,
        *area,
        pixels,
        lv_display_flush_is_last(display),
    };
    context->execute(flush);
}

void DisplayManager::indev_read_cb(lv_indev_t *indev, lv_indev_data_t *data) {
    auto *display = static_cast<DisplayManagerContext *>(
        lv_indev_get_user_data(indev));
    auto touch_mutex = static_cast<SemaphoreHandle_t>(
        display_manager.touch_mutex_);
    if (!display || !touch_mutex) {
        data->state = LV_INDEV_STATE_RELEASED;
        return;
    }

    xSemaphoreTake(touch_mutex, portMAX_DELAY);
    data->point = display->input_point;
    if (display->input_down || display->input_armed) {
        data->state = LV_INDEV_STATE_PRESSED;
        display->input_armed = false;
        display->input_pending = !display->input_down;
    } else {
        data->state = LV_INDEV_STATE_RELEASED;
        display->input_pending = false;
    }
    xSemaphoreGive(touch_mutex);
}

void DisplayManager::touch_event_cb(const bsp_touch_point_t *points, int count,
                                    void *arg) {
    auto *manager = static_cast<DisplayManager *>(arg);
    auto touch_mutex = static_cast<SemaphoreHandle_t>(manager->touch_mutex_);
    if (!touch_mutex) return;
    if (count < 0) count = 0;

    TouchCallback outside_cb = nullptr;
    void *outside_arg = nullptr;
    bool call_outside = false;
    bool schedule_dispatch = false;
    const bsp_touch_point_t *primary = nullptr;
    for (int i = 0; points && i < count; ++i) {
        if (points[i].id == kPrimaryTouchId) {
            primary = &points[i];
            break;
        }
    }

    xSemaphoreTake(touch_mutex, portMAX_DELAY);

    if (primary) {
        if (!manager->active_touch_display_ &&
            !manager->outside_touch_active_) {
            for (uint8_t i = kMaxDisplays; i > 0; --i) {
                DisplayManagerContext *display = manager->displays_[i - 1];
                if (!display) continue;
                if (point_inside(*display, *primary)) {
                    manager->active_touch_display_ = display;
                    break;
                }
            }
            if (!manager->active_touch_display_) {
                manager->outside_touch_active_ = true;
            }
        }

        if (manager->active_touch_display_) {
            DisplayManagerContext &display =
                *manager->active_touch_display_;
            if (!display.input_down) display.input_armed = true;
            display.input_down = true;
            display.input_point = map_touch_point(display, *primary);
            display.input_pending = true;
            schedule_dispatch = true;
        } else {
            call_outside = true;
        }
    } else {
        if (manager->active_touch_display_) {
            DisplayManagerContext &display =
                *manager->active_touch_display_;
            display.input_down = false;
            display.input_pending = true;
            schedule_dispatch = true;
            manager->active_touch_display_ = nullptr;
        }
        if (manager->outside_touch_active_) {
            call_outside = manager->outside_touch_active_;
            if (count == 0) manager->outside_touch_active_ = false;
        }
    }

    if (schedule_dispatch && !manager->input_dispatch_pending_) {
        manager->input_dispatch_pending_ = true;
    } else {
        schedule_dispatch = false;
    }

    if (call_outside) {
        outside_cb = manager->outside_touch_callback_;
        outside_arg = manager->outside_touch_arg_;
    }
    xSemaphoreGive(touch_mutex);

    if (outside_cb) outside_cb(points, count, outside_arg);

    lv_result_t dispatch_result = LV_RESULT_OK;
    if (schedule_dispatch) {
        lv_lock();
        dispatch_result = lv_async_call(input_dispatch_cb, manager);
        lv_unlock();
    }
    if (dispatch_result != LV_RESULT_OK) {
        xSemaphoreTake(touch_mutex, portMAX_DELAY);
        manager->input_dispatch_pending_ = false;
        xSemaphoreGive(touch_mutex);
    }
}

void DisplayManager::input_dispatch_cb(void *arg) {
    auto *manager = static_cast<DisplayManager *>(arg);
    auto touch_mutex = static_cast<SemaphoreHandle_t>(manager->touch_mutex_);
    if (!touch_mutex) return;

    while (true) {
        lv_indev_t *indev = nullptr;

        xSemaphoreTake(touch_mutex, portMAX_DELAY);
        for (uint8_t i = 0; i < kMaxDisplays; ++i) {
            DisplayManagerContext *display = manager->displays_[i];
            if (!display) continue;
            if (display->input_pending) {
                if (lv_display_get_screen_prev(display->display)) {
                    clear_input_state(*display);
                    continue;
                }
                indev = display->indev;
                break;
            }
        }
        if (!indev) manager->input_dispatch_pending_ = false;
        xSemaphoreGive(touch_mutex);

        if (!indev) break;
        lv_indev_read(indev);
    }
}

DisplayManager display_manager;
