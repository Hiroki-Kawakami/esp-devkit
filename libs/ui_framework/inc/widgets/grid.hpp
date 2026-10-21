/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Fixed-size cells laid out in rows on top of lv_list, so rows are pooled and
 * rebound the same way. Columns follow the grid's width.
 */

#pragma once

#include <cstddef>
#include <cstdint>

#include "lvgl.h"

class GridDataSource {
public:
    virtual ~GridDataSource() = default;

    virtual std::size_t cellCount() const = 0;
    virtual int32_t cellWidth() const = 0;
    virtual int32_t cellHeight() const = 0;

    // The least horizontal space between cells and at both edges; whatever
    // the columns leave over is spread evenly.
    virtual int32_t columnSpacing() const { return 0; }
    virtual int32_t rowSpacing() const { return 0; }

    // Called once per pooled cell. The grid sizes and positions the cell.
    virtual lv_obj_t *createCell(lv_obj_t *parent) = 0;
    virtual void bindCell(lv_obj_t *cell, std::size_t index) = 0;

    // Reported for clicks on a cell the data source made clickable.
    virtual void didSelectCell(std::size_t) {}
};

// The data source is borrowed and must outlive the returned LVGL object.
lv_obj_t *lv_grid_create(lv_obj_t *parent, GridDataSource *data_source);

void lv_grid_reload_data(lv_obj_t *grid, bool preserve_position = false);
// Rebinds only that cell, and only while it is pooled.
void lv_grid_reload_cell(lv_obj_t *grid, std::size_t index);
void lv_grid_show_cell(lv_obj_t *grid, std::size_t index,
                       lv_anim_enable_t animation = LV_ANIM_OFF);

// Returns SIZE_MAX when no cell is visible.
std::size_t lv_grid_first_visible_cell(const lv_obj_t *grid);
std::size_t lv_grid_column_count(const lv_obj_t *grid);
