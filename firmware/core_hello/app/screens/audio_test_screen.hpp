/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen_manager.hpp"
#include "bsp.h"

class AudioTestScreen : public Screen {
public:
    void build() override;
    void onAppear() override;
    void onDisappear() override;

    void set_status(const char *text);

private:
    void render_caps();
    void render_volume();
    void cycle_volume();
    void start_play();

    lv_obj_t *caps_label_;
    lv_obj_t *volume_label_;
    lv_obj_t *status_label_;
    int volume_index_ = 2;   /* index into the volume cycle; last step = mute */
};
