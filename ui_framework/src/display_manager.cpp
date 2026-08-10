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

enum class DisplayRenderPath : uint8_t {
    Direct,
    Bitmap,
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
    uint8_t : 4;

    bsp_size_t panel_size = {};
    bsp_size_t logical_size = {};
    bsp_rect_t output_area = {};
    bsp_rotation_t rotation = BSP_ROTATION_0;
    bsp_pixel_format_t format = BSP_PIXEL_FORMAT_RGB565;
    float scale_x = 1.0f;
    float scale_y = 1.0f;

    void *ppa_srm = nullptr;
    bsp_epd_mode_t default_epd_mode = BSP_EPD_MODE_FAST;
    bsp_epd_mode_t next_epd_mode = BSP_EPD_MODE_NONE;
    lv_area_t dirty = {};

    lv_display_t *display = nullptr;
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

esp_err_t composite_surface(DisplayManagerContext &display, void *framebuffer) {
    if (display.render_path != DisplayRenderPath::Surface ||
        !display.buffer0 || !framebuffer) {
        return ESP_ERR_INVALID_STATE;
    }

#ifdef DISPLAY_MANAGER_USE_PPA
    ppa_srm_color_mode_t format;
    if (ppa_format(display.format, &format)) {
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
        operation.in.srm_cm = format;
        operation.out.buffer = framebuffer;
        operation.out.buffer_size = (uint32_t)display.panel_size.width *
            display.panel_size.height * display.bytes_per_pixel;
        operation.out.pic_w = display.panel_size.width;
        operation.out.pic_h = display.panel_size.height;
        operation.out.block_offset_x = display.output_area.origin.x;
        operation.out.block_offset_y = display.output_area.origin.y;
        operation.out.srm_cm = format;
        operation.rotation_angle = ppa_rotation(display.rotation);
        operation.scale_x = display.scale_x;
        operation.scale_y = display.scale_y;
        operation.mode = PPA_TRANS_MODE_BLOCKING;
        return ppa_do_scale_rotate_mirror(client, &operation);
    }
#endif

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
    if (display.render_path == DisplayRenderPath::Surface && !flush.last) return;
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
        logical_size = swaps_axes(config.viewport.rotation)
            ? (bsp_size_t){output_area.size.height, output_area.size.width}
            : output_area.size;
    }
    if (logical_size.width <= 0 || logical_size.height <= 0) {
        return ESP_ERR_INVALID_ARG;
    }

    auto *context = new (std::nothrow) DisplayManagerContext;
    if (!context) return ESP_ERR_NO_MEM;

    uint32_t caps = bsp_display_get_caps();
    bool has_framebuffer = caps & BSP_DISPLAY_CAP_FRAMEBUFFER;
    bool is_epd = caps & BSP_DISPLAY_CAP_EPD_REFRESH;
    bool full_output = same_rect(output_area, full_panel_rect(panel_size));
    bool identity = full_output &&
        logical_size.width == panel_size.width &&
        logical_size.height == panel_size.height &&
        config.viewport.rotation == BSP_ROTATION_0;

    context->panel_size = panel_size;
    context->logical_size = logical_size;
    context->output_area = output_area;
    context->rotation = config.viewport.rotation;
    context->format = bsp_display_get_pixel_format();
    context->bytes_per_pixel = bsp_pixel_format_bytes(context->format);
    context->epd_enabled = is_epd;
    update_scale(*context);

    if (context->bytes_per_pixel == 0) {
        delete context;
        return ESP_ERR_NOT_SUPPORTED;
    }
    if (config.present_mode == DisplayPresentMode::Deferred && !has_framebuffer) {
        delete context;
        return ESP_ERR_NOT_SUPPORTED;
    }

    size_t buffer_bytes = 0;
    lv_display_render_mode_t render_mode;
    if (config.present_mode == DisplayPresentMode::Immediate &&
        has_framebuffer && identity && !is_epd) {
        context->buffer0 = bsp_display_get_frame_buffer(0);
        if (!context->buffer0) {
            delete context;
            return ESP_ERR_INVALID_STATE;
        }
        context->buffer1 = bsp_display_get_frame_buffer(1);
        buffer_bytes = (size_t)logical_size.width * logical_size.height *
            context->bytes_per_pixel;
        render_mode = LV_DISPLAY_RENDER_MODE_DIRECT;
        context->render_path = DisplayRenderPath::Direct;
        context->append(flush_framebuffer);
    } else if (config.present_mode == DisplayPresentMode::Immediate &&
               (!has_framebuffer || is_epd)) {
        if (!scale_is_one(*context)) {
            delete context;
            return ESP_ERR_NOT_SUPPORTED;
        }

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
        uint32_t memory_caps = config.buffer.memory_caps
            ? config.buffer.memory_caps
            : MALLOC_CAP_DEFAULT;
        context->buffer0 = heap_caps_aligned_alloc(4, buffer_bytes, memory_caps);
        if (count == 2 && context->buffer0) {
            context->buffer1 = heap_caps_aligned_alloc(4, buffer_bytes, memory_caps);
        }
        if (!context->buffer0 || (count == 2 && !context->buffer1)) {
            if (context->buffer0) heap_caps_free(context->buffer0);
            if (context->buffer1) heap_caps_free(context->buffer1);
            delete context;
            return ESP_ERR_NO_MEM;
        }
        render_mode = LV_DISPLAY_RENDER_MODE_PARTIAL;
        context->render_path = DisplayRenderPath::Bitmap;
        context->append(map_flush_area);
        context->append(draw_bitmap);
        if (is_epd) {
            context->append(accumulate_dirty);
            context->append(refresh_epd);
        }
    } else {
        buffer_bytes = (size_t)logical_size.width * logical_size.height *
            context->bytes_per_pixel;
        uint32_t memory_caps = config.buffer.memory_caps
            ? config.buffer.memory_caps
            : MALLOC_CAP_DEFAULT;
        context->buffer0 = heap_caps_aligned_alloc(64, buffer_bytes, memory_caps);
        if (!context->buffer0) {
            delete context;
            return ESP_ERR_NO_MEM;
        }
        std::memset(context->buffer0, 0, buffer_bytes);
        render_mode = LV_DISPLAY_RENDER_MODE_DIRECT;
        context->render_path = DisplayRenderPath::Surface;
        if (config.present_mode == DisplayPresentMode::Immediate) {
            context->append(composite_immediate);
            context->append(flush_framebuffer);
        }
    }
    context->append(flush_ready);

    context->display = lv_display_create(logical_size.width, logical_size.height);
    if (!context->display) {
        if (context->render_path != DisplayRenderPath::Direct) {
            heap_caps_free(context->buffer0);
            if (context->buffer1) heap_caps_free(context->buffer1);
        }
        delete context;
        return ESP_ERR_NO_MEM;
    }

    lv_display_set_color_format(context->display,
                                lv_color_format(context->format));
    lv_display_set_buffers(context->display, context->buffer0, context->buffer1,
                           buffer_bytes, render_mode);
    lv_display_set_user_data(context->display, context);
    lv_display_set_flush_cb(context->display, flush_cb);
    if (config.make_default) lv_display_set_default(context->display);

    if (is_epd) bsp_display_set_epd_mode(BSP_EPD_MODE_NONE);

    *out_display = context->display;
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

    bsp_rotation_t previous = context->rotation;
    context->rotation = rotation;
    update_scale(*context);
    if (context->render_path == DisplayRenderPath::Bitmap &&
        !scale_is_one(*context)) {
        context->rotation = previous;
        update_scale(*context);
        return ESP_ERR_NOT_SUPPORTED;
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

esp_err_t DisplayManager::set_visible(lv_display_t *display, bool visible) {
    DisplayManagerContext *context = context_for(display);
    if (!context) return ESP_ERR_INVALID_ARG;

    bool was_visible = context->visible;
    context->visible = visible;
    if (visible && !was_visible) {
        lv_obj_t *screen = lv_display_get_screen_active(display);
        if (screen) lv_obj_invalidate(screen);
    }
    return ESP_OK;
}

esp_err_t DisplayManager::compose(lv_display_t *display,
                                  int framebuffer_index) {
    DisplayManagerContext *context = context_for(display);
    if (!context) return ESP_ERR_INVALID_ARG;

    if (!context->visible) return ESP_OK;
    if (context->render_path == DisplayRenderPath::Bitmap) return ESP_OK;
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

DisplayManager display_manager;
