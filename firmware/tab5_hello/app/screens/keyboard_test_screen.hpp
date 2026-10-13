/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "screen_manager.hpp"
#include "widgets.hpp"
#include "bsp.h"
#include <deque>
#include <memory>
#include <string>

class KeyboardTestScreen : public NavigationScreen {
public:
    void build() override;
    void onEnter() override;
    void onExit() override;

private:
    lv_obj_t *status_ = nullptr;
    lv_obj_t *log_ = nullptr;
    std::deque<std::string> lines_;
    std::weak_ptr<Screen> self_;

    void setMode(bsp_keyboard_mode_t mode);
    void probe();
    void setLeds(uint8_t r, uint8_t g, uint8_t b);
    void handleEvent(const bsp_keyboard_event_t &event);
    void append(const std::string &line);
    void updateStatus();
};
