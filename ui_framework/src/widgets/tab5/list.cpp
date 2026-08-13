/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"

#include <algorithm>
#include <limits>
#include <vector>

namespace {

constexpr std::size_t kOverscanRows = 1;

lv_style_t row_style;
lv_style_t row_pressed_style;
bool row_styles_initialized = false;

void init_row_styles() {
    if (row_styles_initialized) return;
    lv_style_init(&row_style);
    lv_style_set_radius(&row_style, 0);
    lv_style_set_border_side(&row_style, LV_BORDER_SIDE_BOTTOM);
    lv_style_set_border_width(&row_style, 1);
    lv_style_set_border_opa(&row_style, LV_OPA_COVER);
    lv_style_set_border_color(&row_style, lv_separator_default_color());

    lv_style_init(&row_pressed_style);
    lv_style_set_bg_color(&row_pressed_style, lv_color_black());
    lv_style_set_bg_opa(&row_pressed_style, 35);
    row_styles_initialized = true;
}

struct RowSlot {
    lv_obj_t *object = nullptr;
    std::size_t index = SIZE_MAX;
};

struct ListState {
    lv_obj_t *list = nullptr;
    lv_obj_t *extent = nullptr;
    ListDataSource *data_source = nullptr;
    std::vector<RowSlot> pool;
    std::size_t item_count = 0;
    std::size_t first_bound = SIZE_MAX;
    int32_t row_height = 0;
    bool updating = false;
};

ListState *state_for(const lv_obj_t *list) {
    if (!list) return nullptr;
    return static_cast<ListState *>(lv_obj_get_user_data(const_cast<lv_obj_t *>(list)));
}

void bind_visible_rows(ListState *state, bool force);

void row_clicked(lv_event_t *event) {
    auto *state = static_cast<ListState *>(lv_event_get_user_data(event));
    lv_obj_t *target = lv_event_get_target_obj(event);
    for (const RowSlot &slot : state->pool) {
        if (slot.object == target && slot.index != SIZE_MAX) {
            state->data_source->didSelectRow(slot.index);
            return;
        }
    }
}

void ensure_pool(ListState *state) {
    if (state->row_height <= 0) return;
    lv_obj_update_layout(state->list);
    const int32_t height = lv_obj_get_height(state->list);
    if (height <= 0) return;
    const std::size_t required = static_cast<std::size_t>(height / state->row_height)
        + 1 + kOverscanRows * 2;
    while (state->pool.size() < required) {
        lv_obj_t *row = state->data_source->createRow(state->list);
        if (!row) break;
        lv_obj_set_size(row, LV_PCT(100), state->row_height);
        lv_obj_add_flag(row, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_event_cb(row, row_clicked, LV_EVENT_CLICKED, state);
        state->pool.push_back({row, SIZE_MAX});
    }
}

void bind_visible_rows(ListState *state, bool force) {
    if (!state || state->updating || state->pool.empty() || state->row_height <= 0) return;
    int32_t scroll_y = lv_obj_get_scroll_y(state->list);
    if (scroll_y < 0) scroll_y = 0;
    std::size_t first = static_cast<std::size_t>(scroll_y / state->row_height);
    first = first > kOverscanRows ? first - kOverscanRows : 0;
    if (!force && first == state->first_bound) return;

    state->updating = true;
    state->first_bound = first;
    for (std::size_t slot_index = 0; slot_index < state->pool.size(); ++slot_index) {
        RowSlot &slot = state->pool[slot_index];
        const std::size_t item_index = first + slot_index;
        if (item_index >= state->item_count) {
            slot.index = SIZE_MAX;
            lv_obj_add_flag(slot.object, LV_OBJ_FLAG_HIDDEN);
            continue;
        }
        slot.index = item_index;
        lv_obj_remove_flag(slot.object, LV_OBJ_FLAG_HIDDEN);
        const int64_t y = static_cast<int64_t>(item_index) * state->row_height;
        lv_obj_set_pos(slot.object, 0, static_cast<int32_t>(std::min<int64_t>(
            y, std::numeric_limits<int32_t>::max())));
        state->data_source->bindRow(slot.object, item_index);
    }
    state->updating = false;
}

void reload(ListState *state, bool preserve_position) {
    if (!state || state->updating) return;
    const int32_t previous_row_height = state->row_height;
    const std::size_t previous_item_count = state->item_count;
    const int32_t previous_scroll_y = std::max<int32_t>(lv_obj_get_scroll_y(state->list), 0);
    std::size_t first = 0;
    if (preserve_position && previous_row_height > 0) {
        first = static_cast<std::size_t>(previous_scroll_y / previous_row_height);
    }

    state->updating = true;
    state->row_height = std::max<int32_t>(state->data_source->rowHeight(), 0);
    state->item_count = state->data_source->rowCount();
    const bool preserve_exact_position = preserve_position
        && state->row_height == previous_row_height
        && state->item_count == previous_item_count;
    const int64_t extent_height = state->row_height > 0
        ? static_cast<int64_t>(state->item_count) * state->row_height : 0;
    lv_obj_set_height(state->extent, static_cast<int32_t>(std::min<int64_t>(
        extent_height, std::numeric_limits<int32_t>::max())));
    ensure_pool(state);
    for (RowSlot &slot : state->pool) {
        slot.index = SIZE_MAX;
        lv_obj_add_flag(slot.object, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_height(slot.object, state->row_height);
    }
    lv_obj_update_layout(state->list);
    const int64_t requested_y = preserve_exact_position
        ? previous_scroll_y : static_cast<int64_t>(first) * state->row_height;
    const int32_t content_height = lv_obj_get_scroll_bottom(state->list)
        + std::max<int32_t>(lv_obj_get_scroll_y(state->list), 0);
    const int32_t max_scroll = std::max<int32_t>(content_height, 0);
    const int32_t target_y = preserve_position
        ? static_cast<int32_t>(std::min<int64_t>(requested_y, max_scroll)) : 0;
    lv_obj_scroll_to_y(state->list, target_y, LV_ANIM_OFF);
    state->first_bound = SIZE_MAX;
    state->updating = false;
    bind_visible_rows(state, true);
}

void list_scrolled(lv_event_t *event) {
    bind_visible_rows(static_cast<ListState *>(lv_event_get_user_data(event)), false);
}

void list_resized(lv_event_t *event) {
    auto *state = static_cast<ListState *>(lv_event_get_user_data(event));
    if (state->updating) return;
    ensure_pool(state);
    bind_visible_rows(state, true);
}

void list_deleted(lv_event_t *event) {
    delete static_cast<ListState *>(lv_event_get_user_data(event));
}

}  // namespace

lv_obj_t *lv_list_row_create(lv_obj_t *parent) {
    init_row_styles();
    lv_obj_t *row = lv_button_create(parent);
    lv_obj_remove_style_all(row);
    lv_obj_add_style(row, &row_style, 0);
    lv_obj_add_style(row, &row_pressed_style, LV_STATE_PRESSED);
    return row;
}

lv_obj_t *lv_list_create(lv_obj_t *parent, ListDataSource *data_source) {
    if (!parent || !data_source) return nullptr;

    auto *state = new ListState;
    state->data_source = data_source;
    state->list = lv_obj_create(parent);
    lv_obj_remove_style_all(state->list);
    lv_obj_set_size(state->list, LV_PCT(100), LV_SIZE_CONTENT);
    lv_obj_set_flex_grow(state->list, 1);
    lv_obj_set_scroll_dir(state->list, LV_DIR_VER);
    lv_obj_set_scrollbar_mode(state->list, LV_SCROLLBAR_MODE_AUTO);
    lv_obj_set_user_data(state->list, state);
    lv_obj_add_event_cb(state->list, list_scrolled, LV_EVENT_SCROLL, state);
    lv_obj_add_event_cb(state->list, list_resized, LV_EVENT_SIZE_CHANGED, state);
    lv_obj_add_event_cb(state->list, list_deleted, LV_EVENT_DELETE, state);

    state->extent = lv_obj_create(state->list);
    lv_obj_remove_style_all(state->extent);
    lv_obj_set_size(state->extent, 1, 0);
    lv_obj_set_pos(state->extent, 0, 0);
    lv_obj_remove_flag(state->extent, LV_OBJ_FLAG_SCROLLABLE);

    reload(state, false);
    return state->list;
}

void lv_list_reload_data(lv_obj_t *list, bool preserve_position) {
    reload(state_for(list), preserve_position);
}

void lv_list_show_row(lv_obj_t *list, std::size_t index, lv_anim_enable_t animation) {
    ListState *state = state_for(list);
    if (!state || index >= state->item_count || state->row_height <= 0) return;
    const int64_t y = static_cast<int64_t>(index) * state->row_height;
    lv_obj_scroll_to_y(state->list, static_cast<int32_t>(std::min<int64_t>(
        y, std::numeric_limits<int32_t>::max())), animation);
    if (animation == LV_ANIM_OFF) bind_visible_rows(state, true);
}

std::size_t lv_list_first_visible_row(const lv_obj_t *list) {
    ListState *state = state_for(list);
    if (!state || state->item_count == 0 || state->row_height <= 0) return SIZE_MAX;
    const int32_t scroll_y = std::max<int32_t>(lv_obj_get_scroll_y(state->list), 0);
    return std::min<std::size_t>(static_cast<std::size_t>(scroll_y / state->row_height),
                                 state->item_count - 1);
}

lv_obj_t *lv_list_row_for_index(const lv_obj_t *list, std::size_t index) {
    ListState *state = state_for(list);
    if (!state) return nullptr;
    for (const RowSlot &slot : state->pool) {
        if (slot.index == index) return slot.object;
    }
    return nullptr;
}
