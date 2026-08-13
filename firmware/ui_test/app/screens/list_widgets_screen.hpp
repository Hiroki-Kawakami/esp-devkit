/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "screen_manager.hpp"
#include "widgets.hpp"

#include <cstddef>
#include <string>

class ListWidgetsScreen : public ListDataSource, public NavigationScreen {
public:
    void build() override;

    std::size_t rowCount() const override;
    int32_t rowHeight() const override;
    lv_obj_t *createRow(lv_obj_t *parent) override;
    void bindRow(lv_obj_t *row, std::size_t index) override;
    void didSelectRow(std::size_t index) override;
    std::string statusBarText(lv_obj_t *list, int page, int total_pages) override;

private:
    static constexpr std::size_t kRowCount = 95;

    lv_obj_t *list_ = nullptr;
    std::size_t selected_ = SIZE_MAX;
};
