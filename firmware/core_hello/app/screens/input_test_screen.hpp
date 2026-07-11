/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <string>
#include <vector>
#include "screen_manager.hpp"
#include "bsp.h"

class InputTestScreen : public Screen {
public:
    void build() override;
    void onAppear() override;
    void onDisappear() override;

private:
    void on_touch(const bsp_touch_point_t *points, int count);
    void on_button(uint8_t id, const char *event);
    void add_event(const char *text);
    void render_log();

    lv_obj_t *touch_label_;
    lv_obj_t *log_label_;
    std::vector<std::string> events_;
    int prev_touch_count_ = 0;
};
