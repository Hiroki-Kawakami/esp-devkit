/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Fixed-height, reusable list interface.
 *
 * The platform implementation decides how rows are presented:
 *
 * - paper: a page-sized row pool and a status bar with page controls
 * - tab5:  a viewport-sized row pool rebound while the list scrolls
 *
 * Platform implementations use their fixed status-bar height and overscan.
 * The paper row count per page is derived from the available height and
 * ListDataSource::rowHeight().
 *
 * In either case createRow() is called only when a pool slot is allocated.
 * bindRow() is called whenever that slot starts representing another index.
 */

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

#include "lvgl.h"

class ListDataSource {
public:
    virtual ~ListDataSource() = default;

    // The number of logical rows currently available.
    virtual std::size_t rowCount() const = 0;

    // Minimum row height. Tab5 uses it as-is; paper may increase it so a page
    // fills the available height with equal-sized rows.
    virtual int32_t rowHeight() const = 0;

    // Creates the root object for one reusable row. Called once per pool slot,
    // not once per logical row. The returned object must be a child of parent.
    virtual lv_obj_t *createRow(lv_obj_t *parent) = 0;

    // Updates all index-dependent state of an existing row. Implementations
    // must not assume that the row was previously bound to a nearby index.
    virtual void bindRow(lv_obj_t *row, std::size_t index) = 0;

    // The list registers a click event callback on the row root returned by
    // createRow() and reports its currently bound logical index here. It does
    // not make the row clickable; createRow()/bindRow() control selection by
    // adding or removing LV_OBJ_FLAG_CLICKABLE on the row root.
    virtual void didSelectRow(std::size_t) {}

    // Paper only. Returns the text shown in the status bar. page is one-based;
    // both page and total_pages are zero when the list is empty.
    virtual std::string statusBarText(lv_obj_t *, int, int) {
        return {};
    }
};

// Creates a row root with the platform-specific list interaction style.
lv_obj_t *lv_list_row_create(lv_obj_t *parent);

// The data source is borrowed and must outlive the returned LVGL object.
// Deleting the LVGL object releases the internal row pool automatically.
lv_obj_t *lv_list_create(lv_obj_t *parent, ListDataSource *data_source);

// Re-reads rowCount(), updates the scroll extent/page count, and rebinds the
// active row pool. Use this after inserting, deleting, or replacing data.
// By default, returns to the first row/page.
void lv_list_reload_data(lv_obj_t *list, bool preserve_position = false);

// Tab5 scrolls to the row; paper switches to the page containing the row.
void lv_list_show_row(lv_obj_t *list, std::size_t index,
                      lv_anim_enable_t animation = LV_ANIM_OFF);

// Returns SIZE_MAX when no row is visible or index is not currently pooled.
std::size_t lv_list_first_visible_row(const lv_obj_t *list);
lv_obj_t *lv_list_row_for_index(const lv_obj_t *list, std::size_t index);
