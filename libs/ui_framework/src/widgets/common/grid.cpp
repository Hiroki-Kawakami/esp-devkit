/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"

#include <algorithm>

namespace {

class GridAdapter : public ListDataSource {
public:
    explicit GridAdapter(GridDataSource *source) : source_(source) {}

    GridDataSource *source() const { return source_; }
    std::size_t columns() const { return columns_; }

    bool update_columns(int32_t width) {
        const int32_t spacing = source_->columnSpacing();
        const std::size_t columns =
            std::max<int32_t>(1, (width - spacing) / (source_->cellWidth() + spacing));
        if (columns == columns_) return false;
        columns_ = columns;
        return true;
    }

    std::size_t rowCount() const override {
        return (source_->cellCount() + columns_ - 1) / columns_;
    }

    int32_t rowHeight() const override {
        return source_->cellHeight() + source_->rowSpacing();
    }

    lv_obj_t *createRow(lv_obj_t *parent) override {
        lv_obj_t *row = lv_container_create(parent);
        lv_obj_set_clickable(row, false);
        ensure_cells(row);
        return row;
    }

    void bindRow(lv_obj_t *row, std::size_t index) override {
        ensure_cells(row);
        lv_obj_set_user_data(row, reinterpret_cast<void *>(index));

        const int32_t width = lv_obj_get_content_width(lv_obj_get_parent(row));
        const int32_t cell_width = source_->cellWidth();
        const int32_t columns = static_cast<int32_t>(columns_);
        const int32_t gap = std::max<int32_t>((width - columns * cell_width) / (columns + 1), 0);
        const uint32_t count = lv_obj_get_child_count(row);
        for (uint32_t i = 0; i < count; i++) {
            lv_obj_t *cell = lv_obj_get_child(row, i);
            const std::size_t cell_index = index * columns_ + i;
            if (i >= columns_ || cell_index >= source_->cellCount()) {
                lv_obj_set_hidden(cell, true);
                continue;
            }
            lv_obj_set_hidden(cell, false);
            lv_obj_set_pos(cell, gap + static_cast<int32_t>(i) * (cell_width + gap),
                           source_->rowSpacing() / 2);
            source_->bindCell(cell, cell_index);
        }
    }

private:
    GridDataSource *source_;
    std::size_t columns_ = 1;

    void ensure_cells(lv_obj_t *row) {
        while (lv_obj_get_child_count(row) < columns_) {
            lv_obj_t *cell = source_->createCell(row);
            if (!cell) break;
            lv_obj_set_size(cell, source_->cellWidth(), source_->cellHeight());
            lv_obj_add_event_cb(cell, cell_clicked, LV_EVENT_CLICKED, this);
        }
    }

    static void cell_clicked(lv_event_t *event) {
        auto *adapter = static_cast<GridAdapter *>(lv_event_get_user_data(event));
        lv_obj_t *cell = lv_event_get_current_target_obj(event);
        const auto row = reinterpret_cast<std::size_t>(lv_obj_get_user_data(lv_obj_get_parent(cell)));
        const std::size_t index = row * adapter->columns_ + lv_obj_get_index(cell);
        if (index < adapter->source_->cellCount()) adapter->source_->didSelectCell(index);
    }
};

void grid_deleted(lv_event_t *event) {
    delete static_cast<GridAdapter *>(lv_event_get_user_data(event));
}

GridAdapter *adapter_for(const lv_obj_t *grid) {
    if (!grid) return nullptr;
    auto *obj = const_cast<lv_obj_t *>(grid);
    const uint32_t count = lv_obj_get_event_count(obj);
    for (uint32_t i = 0; i < count; i++) {
        lv_event_dsc_t *dsc = lv_obj_get_event_dsc(obj, i);
        if (lv_event_dsc_get_cb(dsc) == grid_deleted) {
            return static_cast<GridAdapter *>(lv_event_dsc_get_user_data(dsc));
        }
    }
    return nullptr;
}

void grid_resized(lv_event_t *event) {
    auto *adapter = static_cast<GridAdapter *>(lv_event_get_user_data(event));
    lv_obj_t *grid = lv_event_get_current_target_obj(event);
    const std::size_t first = lv_grid_first_visible_cell(grid);
    if (!adapter->update_columns(lv_obj_get_content_width(grid))) return;
    lv_list_reload_data(grid);
    if (first != SIZE_MAX) lv_list_show_row(grid, first / adapter->columns());
}

}  // namespace

lv_obj_t *lv_grid_create(lv_obj_t *parent, GridDataSource *data_source) {
    if (!parent || !data_source) return nullptr;

    auto *adapter = new GridAdapter(data_source);
    lv_obj_update_layout(parent);
    adapter->update_columns(lv_obj_get_content_width(parent));
    lv_obj_t *grid = lv_list_create(parent, adapter);
    if (!grid) {
        delete adapter;
        return nullptr;
    }
    lv_obj_add_event_cb(grid, grid_deleted, LV_EVENT_DELETE, adapter);
    lv_obj_add_event_cb(grid, grid_resized, LV_EVENT_SIZE_CHANGED, adapter);
    return grid;
}

void lv_grid_reload_data(lv_obj_t *grid, bool preserve_position) {
    if (!adapter_for(grid)) return;
    lv_list_reload_data(grid, preserve_position);
}

void lv_grid_reload_cell(lv_obj_t *grid, std::size_t index) {
    GridAdapter *adapter = adapter_for(grid);
    if (!adapter || index >= adapter->source()->cellCount()) return;
    lv_obj_t *row = lv_list_row_for_index(grid, index / adapter->columns());
    if (!row) return;
    lv_obj_t *cell = lv_obj_get_child(row, static_cast<int32_t>(index % adapter->columns()));
    if (cell && !lv_obj_has_flag(cell, LV_OBJ_FLAG_HIDDEN)) adapter->source()->bindCell(cell, index);
}

void lv_grid_show_cell(lv_obj_t *grid, std::size_t index, lv_anim_enable_t animation) {
    GridAdapter *adapter = adapter_for(grid);
    if (!adapter) return;
    lv_list_show_row(grid, index / adapter->columns(), animation);
}

std::size_t lv_grid_first_visible_cell(const lv_obj_t *grid) {
    GridAdapter *adapter = adapter_for(grid);
    if (!adapter) return SIZE_MAX;
    const std::size_t row = lv_list_first_visible_row(grid);
    return row == SIZE_MAX ? SIZE_MAX : row * adapter->columns();
}

std::size_t lv_grid_column_count(const lv_obj_t *grid) {
    GridAdapter *adapter = adapter_for(grid);
    return adapter ? adapter->columns() : 0;
}
