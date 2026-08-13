/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "widgets.hpp"

#include <algorithm>
#include <limits>
#include <string>
#include <vector>

namespace {

constexpr int32_t kStatusBarHeight = 81;

struct RowSlot {
    lv_obj_t *container = nullptr;
    lv_obj_t *object = nullptr;
    lv_obj_t *separator = nullptr;
    std::size_t index = SIZE_MAX;
};

struct ListState {
    lv_obj_t *list = nullptr;
    lv_obj_t *rows = nullptr;
    lv_obj_t *status_label = nullptr;
    lv_obj_t *previous_button = nullptr;
    lv_obj_t *next_button = nullptr;
    ListDataSource *data_source = nullptr;
    std::vector<RowSlot> pool;
    std::size_t item_count = 0;
    std::size_t page = 0;
    std::size_t page_count = 0;
    std::size_t rows_per_page = 0;
    int32_t minimum_row_height = 0;
    bool updating = false;
};

lv_style_t row_style;
lv_style_t row_pressed_style;
bool row_styles_initialized = false;

void init_row_styles() {
    if (row_styles_initialized) return;
    lv_style_init(&row_style);
    lv_style_set_border_width(&row_style, 1);
    lv_style_set_border_opa(&row_style, LV_OPA_COVER);
    lv_style_set_border_color(&row_style, lv_color_white());
    lv_style_set_radius(&row_style, 0);

    lv_style_init(&row_pressed_style);
    lv_style_set_border_width(&row_pressed_style, 1);
    lv_style_set_border_color(&row_pressed_style, lv_color_black());

    row_styles_initialized = true;
}

ListState *state_for(const lv_obj_t *list) {
    if (!list) return nullptr;
    return static_cast<ListState *>(lv_obj_get_user_data(const_cast<lv_obj_t *>(list)));
}

void bind_page(ListState *state);
void reload(ListState *state, bool preserve_position);

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

void create_pool(ListState *state) {
    while (state->pool.size() < state->rows_per_page) {
        lv_obj_t *container = lv_container_create(state->rows);
        lv_obj_set_size(container, LV_PCT(100), 0);
        lv_obj_set_flex_grow(container, 1);
        lv_obj_remove_flag(container, LV_OBJ_FLAG_SCROLLABLE);

        lv_obj_t *row = state->data_source->createRow(container);
        if (!row) {
            lv_obj_delete(container);
            break;
        }
        lv_obj_set_size(row, LV_PCT(100), LV_PCT(100));
        lv_obj_add_flag(row, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_event_cb(row, row_clicked, LV_EVENT_CLICKED, state);
        lv_obj_t *separator = lv_hor_separator_create(state->rows);
        lv_obj_add_flag(separator, LV_OBJ_FLAG_HIDDEN);
        lv_obj_remove_flag(separator, LV_OBJ_FLAG_CLICKABLE);
        state->pool.push_back({container, row, separator, SIZE_MAX});
    }
    while (state->pool.size() > state->rows_per_page) {
        lv_obj_delete(state->pool.back().separator);
        lv_obj_delete(state->pool.back().container);
        state->pool.pop_back();
    }
}

void bind_page(ListState *state) {
    state->updating = true;
    const std::size_t first = state->page * state->rows_per_page;
    for (std::size_t slot_index = 0; slot_index < state->pool.size(); ++slot_index) {
        RowSlot &slot = state->pool[slot_index];
        const std::size_t item_index = first + slot_index;
        if (item_index >= state->item_count) {
            slot.index = SIZE_MAX;
            lv_obj_add_flag(slot.object, LV_OBJ_FLAG_HIDDEN);
            if (slot_index + 1 < state->rows_per_page) {
                lv_obj_set_style_bg_opa(slot.separator, LV_OPA_TRANSP, 0);
                lv_obj_remove_flag(slot.separator, LV_OBJ_FLAG_HIDDEN);
            } else {
                lv_obj_add_flag(slot.separator, LV_OBJ_FLAG_HIDDEN);
            }
            continue;
        }
        slot.index = item_index;
        lv_obj_remove_flag(slot.object, LV_OBJ_FLAG_HIDDEN);
        state->data_source->bindRow(slot.object, item_index);
        if (slot_index + 1 < state->rows_per_page) {
            lv_obj_set_style_bg_opa(slot.separator, LV_OPA_COVER, 0);
            lv_obj_remove_flag(slot.separator, LV_OBJ_FLAG_HIDDEN);
        } else {
            lv_obj_add_flag(slot.separator, LV_OBJ_FLAG_HIDDEN);
        }
    }

    const int page = state->page_count == 0 ? 0 : static_cast<int>(std::min<std::size_t>(
        state->page + 1, static_cast<std::size_t>(std::numeric_limits<int>::max())));
    const int total_pages = static_cast<int>(std::min<std::size_t>(
        state->page_count, static_cast<std::size_t>(std::numeric_limits<int>::max())));
    std::string text = state->data_source->statusBarText(state->list, page, total_pages);
    lv_label_set_text(state->status_label, text.c_str());

    if (state->page > 0) {
        lv_obj_remove_flag(state->previous_button, LV_OBJ_FLAG_HIDDEN);
    } else {
        lv_obj_add_flag(state->previous_button, LV_OBJ_FLAG_HIDDEN);
    }
    if (state->page + 1 < state->page_count) {
        lv_obj_remove_flag(state->next_button, LV_OBJ_FLAG_HIDDEN);
    } else {
        lv_obj_add_flag(state->next_button, LV_OBJ_FLAG_HIDDEN);
    }
    state->updating = false;
}

void reload(ListState *state, bool preserve_position) {
    if (!state || state->updating) return;
    const std::size_t first = preserve_position && state->rows_per_page > 0
        ? state->page * state->rows_per_page : 0;

    state->updating = true;
    lv_obj_update_layout(state->list);
    state->minimum_row_height = std::max<int32_t>(state->data_source->rowHeight(), 0);
    const int32_t available_height = lv_obj_get_height(state->rows);
    state->rows_per_page = state->minimum_row_height > 0 && available_height > 0
        ? static_cast<std::size_t>((static_cast<int64_t>(available_height) + 1) /
                                   (static_cast<int64_t>(state->minimum_row_height) + 1)) : 0;
    state->item_count = state->data_source->rowCount();
    state->page_count = state->rows_per_page == 0 ? 0
        : state->item_count / state->rows_per_page
            + (state->item_count % state->rows_per_page != 0 ? 1 : 0);
    if (state->page_count == 0) {
        state->page = 0;
    } else {
        state->page = std::min(first / state->rows_per_page, state->page_count - 1);
    }
    create_pool(state);
    state->updating = false;
    bind_page(state);
}

void rows_resized(lv_event_t *event) {
    auto *state = static_cast<ListState *>(lv_event_get_user_data(event));
    reload(state, true);
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

    init_row_styles();
    auto *state = new ListState;
    state->data_source = data_source;
    state->list = lv_obj_create(parent);
    lv_obj_remove_style_all(state->list);
    lv_obj_set_size(state->list, LV_PCT(100), LV_SIZE_CONTENT);
    lv_obj_set_flex_grow(state->list, 1);
    lv_obj_set_flex_flow(state->list, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_pad_row(state->list, 0, 0);
    lv_obj_remove_flag(state->list, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_set_user_data(state->list, state);
    lv_obj_add_event_cb(state->list, list_deleted, LV_EVENT_DELETE, state);

    state->rows = lv_obj_create(state->list);
    lv_obj_remove_style_all(state->rows);
    lv_obj_set_size(state->rows, LV_PCT(100), LV_SIZE_CONTENT);
    lv_obj_set_flex_grow(state->rows, 1);
    lv_obj_set_flex_flow(state->rows, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_style_pad_row(state->rows, 0, 0);
    lv_obj_remove_flag(state->rows, LV_OBJ_FLAG_SCROLLABLE);

    lv_obj_t *status = lv_obj_create(state->list);
    lv_obj_remove_style_all(status);
    lv_obj_set_size(status, LV_PCT(100), kStatusBarHeight);
    lv_obj_set_style_border_side(status, LV_BORDER_SIDE_TOP, 0);
    lv_obj_set_style_border_width(status, 2, 0);
    lv_obj_set_style_border_color(status, lv_color_black(), 0);
    lv_obj_remove_flag(status, LV_OBJ_FLAG_SCROLLABLE);

    state->status_label = lv_label_create(status);
    lv_obj_set_style_text_font(state->status_label, lv_widgets_body_font(), 0);
    lv_obj_center(state->status_label);

    state->previous_button = lv_button_create(status, LV_BUTTON_STYLE_PLAIN);
    lv_obj_set_size(state->previous_button, 100, 60);
    lv_obj_align(state->previous_button, LV_ALIGN_LEFT_MID, 12, 0);
    lv_button_set_text(state->previous_button, "Prev", lv_widgets_body_font());
    lv_obj_add_event_cb(state->previous_button, [](lv_event_t *event) {
        auto *state = static_cast<ListState *>(lv_event_get_user_data(event));
        if (state->page == 0) return;
        --state->page;
        bind_page(state);
    }, LV_EVENT_CLICKED, state);

    state->next_button = lv_button_create(status, LV_BUTTON_STYLE_PLAIN);
    lv_obj_set_size(state->next_button, 100, 60);
    lv_obj_align(state->next_button, LV_ALIGN_RIGHT_MID, -12, 0);
    lv_button_set_text(state->next_button, "Next", lv_widgets_body_font());
    lv_obj_add_event_cb(state->next_button, [](lv_event_t *event) {
        auto *state = static_cast<ListState *>(lv_event_get_user_data(event));
        if (state->page + 1 >= state->page_count) return;
        ++state->page;
        bind_page(state);
    }, LV_EVENT_CLICKED, state);

    lv_obj_add_event_cb(state->rows, rows_resized, LV_EVENT_SIZE_CHANGED, state);
    reload(state, false);
    return state->list;
}

void lv_list_reload_data(lv_obj_t *list, bool preserve_position) {
    reload(state_for(list), preserve_position);
}

void lv_list_show_row(lv_obj_t *list, std::size_t index, lv_anim_enable_t animation) {
    (void)animation;
    ListState *state = state_for(list);
    if (!state || index >= state->item_count || state->rows_per_page == 0) return;
    state->page = index / state->rows_per_page;
    bind_page(state);
}

std::size_t lv_list_first_visible_row(const lv_obj_t *list) {
    ListState *state = state_for(list);
    if (!state || state->item_count == 0 || state->rows_per_page == 0) return SIZE_MAX;
    return state->page * state->rows_per_page;
}

lv_obj_t *lv_list_row_for_index(const lv_obj_t *list, std::size_t index) {
    ListState *state = state_for(list);
    if (!state) return nullptr;
    for (const RowSlot &slot : state->pool) {
        if (slot.index == index) return slot.object;
    }
    return nullptr;
}
