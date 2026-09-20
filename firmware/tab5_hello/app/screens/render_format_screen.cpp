/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "render_format_screen.hpp"
#include "display_manager.hpp"
#include "esp_log.h"

static const char *TAG = "render_format";

static const char *panel_format_name(bsp_pixel_format_t format) {
    switch (format) {
        case BSP_PIXEL_FORMAT_L8:     return "L8";
        case BSP_PIXEL_FORMAT_RGB888: return "RGB888";
        case BSP_PIXEL_FORMAT_RGB565: return "RGB565";
        default:                      return "default";
    }
}

static const char *render_format_name(lv_color_format_t format) {
    switch (format) {
        case LV_COLOR_FORMAT_L8:     return "L8";
        case LV_COLOR_FORMAT_RGB888: return "RGB888";
        case LV_COLOR_FORMAT_RGB565: return "RGB565";
        default:                     return "?";
    }
}

static lv_obj_t *add_button(lv_obj_t *parent, const char *text) {
    auto button = lv_button_create(parent);
    lv_obj_set_width(button, LV_PCT(100));
    auto title = lv_label_create(button);
    lv_obj_center(title);
    lv_label_set_text(title, text);
    return button;
}

static void fill_gradient(lv_obj_t *parent) {
    auto bar = lv_obj_create(parent);
    lv_obj_set_size(bar, LV_PCT(100), 120);
    lv_obj_set_style_border_width(bar, 0, 0);
    lv_obj_set_style_pad_all(bar, 0, 0);
    lv_obj_set_style_radius(bar, 0, 0);
    lv_obj_set_style_bg_grad_dir(bar, LV_GRAD_DIR_HOR, 0);
    lv_obj_set_style_bg_color(bar, lv_color_black(), 0);
    lv_obj_set_style_bg_grad_color(bar, lv_color_hex(0x2040FF), 0);
}

void RenderFormatScreen::build() {
    createNavigation("Render Format");
    entry_format_ = bsp_display_get_pixel_format();

    status_ = lv_label_create(contents_);
    lv_obj_set_style_text_color(status_, lv_color_black(), 0);
    updateStatus("entry", ESP_OK);

    fill_gradient(contents_);

    auto panel_565 = add_button(contents_, "Panel RGB565");
    lv_obj_add_event_fn(panel_565, LV_EVENT_CLICKED, [this](lv_event_t *) {
        setPanelFormat(BSP_PIXEL_FORMAT_RGB565);
    });
    auto panel_888 = add_button(contents_, "Panel RGB888");
    lv_obj_add_event_fn(panel_888, LV_EVENT_CLICKED, [this](lv_event_t *) {
        setPanelFormat(BSP_PIXEL_FORMAT_RGB888);
    });
    auto render_565 = add_button(contents_, "Render RGB565");
    lv_obj_add_event_fn(render_565, LV_EVENT_CLICKED, [this](lv_event_t *) {
        setRenderFormat(LV_COLOR_FORMAT_RGB565);
    });
    auto render_888 = add_button(contents_, "Render RGB888");
    lv_obj_add_event_fn(render_888, LV_EVENT_CLICKED, [this](lv_event_t *) {
        setRenderFormat(LV_COLOR_FORMAT_RGB888);
    });
    auto rotate_button = add_button(contents_, "Rotate");
    lv_obj_add_event_fn(rotate_button, LV_EVENT_CLICKED, [this](lv_event_t *) {
        rotate();
    });
}

void RenderFormatScreen::onExit() {
    if (rotation_ != BSP_ROTATION_0) {
        rotation_ = BSP_ROTATION_0;
        display_manager.set_rotation(lv_obj_get_display(root_), rotation_);
    }
    if (bsp_display_get_pixel_format() != entry_format_) {
        setPanelFormat(entry_format_);
    }
}

void RenderFormatScreen::setPanelFormat(bsp_pixel_format_t format) {
    lv_display_t *display = lv_obj_get_display(root_);
    const lv_color_format_t render = lv_display_get_color_format(display);

    esp_err_t err = bsp_display_reconfigure(format, 0);
    if (err == ESP_OK) err = display_manager.set_color_format(display, render);
    /* The render format the display carried may be unreachable from the new
     * panel format, which leaves it bound to a panel it cannot draw to. */
    if (err != ESP_OK) err = display_manager.set_color_format(display);
    updateStatus("panel", err);
}

void RenderFormatScreen::setRenderFormat(lv_color_format_t format) {
    esp_err_t err = display_manager.set_color_format(lv_obj_get_display(root_), format);
    updateStatus("render", err);
}

void RenderFormatScreen::rotate() {
    rotation_ = (bsp_rotation_t)((rotation_ + 1) & 3);
    esp_err_t err = display_manager.set_rotation(lv_obj_get_display(root_), rotation_);
    updateStatus("rotate", err);
}

void RenderFormatScreen::updateStatus(const char *action, esp_err_t err) {
    lv_display_t *display = lv_obj_get_display(root_);
    ESP_LOGI(TAG, "%s: %s (panel %s, render %s)", action, esp_err_to_name(err),
             panel_format_name(bsp_display_get_pixel_format()),
             render_format_name(lv_display_get_color_format(display)));
    if (!status_) return;
    lv_label_set_text_fmt(status_, "panel %s / render %s\n%s: %s",
                          panel_format_name(bsp_display_get_pixel_format()),
                          render_format_name(lv_display_get_color_format(display)),
                          action, esp_err_to_name(err));
}
