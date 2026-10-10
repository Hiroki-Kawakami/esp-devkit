/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Screen — base class for a single full-screen LVGL view managed by
 * ScreenManager. Subclasses build their widget tree in build() and react to
 * lifecycle events.
 */

#pragma once
#include <memory>
#include "lvgl.hpp"

class Screen : public std::enable_shared_from_this<Screen> {
public:
    lv_obj_t *root_;

    Screen(): root_{nullptr} {}
    virtual ~Screen() { lv_obj_delete(root_); }

    // Build Screen Components
    virtual lv_theme_t *theme() {
#if LV_USE_THEME_DEFAULT
        return lv_theme_default_get();
#elif LV_USE_THEME_SIMPLE
        return lv_theme_simple_get();
#elif LV_USE_THEME_MONO
        return lv_theme_mono_get();
#else
        return nullptr;
#endif
    }
    virtual void build() = 0;

    // Lifecycle Events
    virtual void onEnter()  {}
    virtual void onExit()   {}
    virtual void onAppear()  {}
    virtual void onDisappear() {}

    bool exited() const { return exited_; }

private:
    friend class ScreenManager;
    bool exited_ = false;
};
