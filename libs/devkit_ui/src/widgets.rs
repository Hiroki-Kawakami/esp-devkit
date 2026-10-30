/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Widgets are functions building elements from the theme. State stays with the
 * caller, which passes it in and hears about changes through messages.
 */

use crate::element::{Children, Drag, Element, Ui};
use crate::style::{Align, Justify, Role};
use crate::theme::WidgetLook;

const SWITCH_WIDTH: i16 = 64;
const SWITCH_HEIGHT: i16 = 36;
const SWITCH_THUMB: i16 = 28;
const CHECK_SIZE: i16 = 28;
const SLIDER_HEIGHT: i16 = 48;
const SLIDER_TRACK: i16 = 8;
const MENU_WIDTH: i16 = 380;
const MODAL_WIDTH: i16 = 560;
const VALUE_RANGE: u16 = 255;

/// `look` is usually `ui.theme().looks.button` or `.button_secondary`.
pub fn button<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, look: &'static WidgetLook, on_tap: M) -> Element<'a, M> {
    let m = &ui.theme().metrics;
    ui.row([ui.text(ui.theme().fonts.body, label)])
        .look(look)
        .height(m.touch_size + 8)
        .padding_x(m.padding)
        .radius(m.radius)
        .align(Align::Center)
        .justify(Justify::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

pub fn icon_button<'a, M: Copy>(ui: &'a Ui<M>, icon: char, on_tap: M) -> Element<'a, M> {
    let theme = ui.theme();
    let size = theme.metrics.touch_size;
    ui.row([ui.icon(theme.fonts.icons, icon)])
        .look(theme.looks.icon_button)
        .size(size, size)
        .radius((size / 2) as u8)
        .align(Align::Center)
        .justify(Justify::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

/// `leading` and `trailing` are icon characters.
pub fn list_item<'a, M: Copy>(ui: &'a Ui<M>, leading: Option<char>, title: &'a str, meta: Option<&'a str>, trailing: Option<char>, on_tap: M) -> Element<'a, M> {
    let theme = ui.theme();
    let (c, m) = (&theme.colors, &theme.metrics);
    let lead = leading.map_or(ui.empty(), |icon| ui.icon(theme.fonts.icons, icon).color(c.muted));
    let tail = trailing.map_or(ui.empty(), |icon| ui.icon(theme.fonts.icons, icon).color(c.divider));
    let detail = meta.map_or(ui.empty(), |text| ui.text(theme.fonts.caption, text).color(c.muted));
    ui.row([lead, ui.column([ui.text(theme.fonts.body, title).ellipsis(), detail]).gap(4).grow(1), tail])
        .look(theme.looks.row)
        .height(m.row_height)
        .padding_x(m.padding)
        .gap(m.padding)
        .align(Align::Center)
        .border_bottom(c.divider)
        .role(Role::Button)
        .on_tap(on_tap)
}

/// `back` is an icon button before the title; `actions` follow it.
pub fn top_bar<'a, M: Copy + 'static>(ui: &'a Ui<M>, back: Option<(char, M)>, title: &'a str, actions: Element<'a, M>) -> Element<'a, M> {
    let theme = ui.theme();
    let lead = back.map_or(ui.empty(), |(icon, msg)| icon_button(ui, icon, msg).id("back"));
    ui.row([lead, ui.text(theme.fonts.title, title).ellipsis().grow(1), actions])
        .height(theme.metrics.bar_height)
        .padding_x(theme.metrics.gap)
        .gap(theme.metrics.gap)
        .align(Align::Center)
        .role(Role::Header)
}

pub fn switch<'a, M: Copy>(ui: &'a Ui<M>, on: bool, on_tap: M) -> Element<'a, M> {
    let c = &ui.theme().colors;
    let inset = (SWITCH_HEIGHT - SWITCH_THUMB) / 2;
    let x = if on { SWITCH_WIDTH - SWITCH_THUMB - inset } else { inset };
    ui.column([ui.column([]).size(SWITCH_THUMB, SWITCH_THUMB).radius((SWITCH_THUMB / 2) as u8).background(c.surface).absolute(Some(x), Some(inset), None, None)])
        .size(SWITCH_WIDTH, SWITCH_HEIGHT)
        .radius((SWITCH_HEIGHT / 2) as u8)
        .background(if on { c.primary } else { c.divider })
        .checked(on)
        .role(Role::Button)
        .on_tap(on_tap)
}

pub fn checkbox<'a, M: Copy>(ui: &'a Ui<M>, checked: bool, on_tap: M) -> Element<'a, M> {
    let c = &ui.theme().colors;
    let mark = if checked { ui.column([]).size(CHECK_SIZE - 12, CHECK_SIZE - 12).radius(4).background(c.on_primary).absolute(Some(6), Some(6), None, None) } else { ui.empty() };
    ui.column([mark]).size(CHECK_SIZE, CHECK_SIZE).radius(6).background(if checked { c.primary } else { c.divider }).checked(checked).role(Role::Button).on_tap(on_tap)
}

/// Where a drag lands on a slider, 0..=255.
pub fn slider_value(drag: Drag) -> u8 {
    let width = i32::from(drag.width.max(1));
    (i32::from(drag.x).clamp(0, width) * i32::from(VALUE_RANGE) / width) as u8
}

/// `value` is 0..=255. Takes the width it is given.
pub fn slider<'a, M: Copy>(ui: &'a Ui<M>, value: u8, on_drag: fn(Drag) -> M) -> Element<'a, M> {
    let c = &ui.theme().colors;
    let top = (SLIDER_HEIGHT - SLIDER_TRACK) / 2;
    let radius = (SLIDER_TRACK / 2) as u8;
    ui.column([
        ui.column([]).height(SLIDER_TRACK).radius(radius).background(c.divider).absolute(Some(0), Some(top), Some(0), None),
        ui.column([]).width_fraction(u16::from(value), VALUE_RANGE).min_width(SLIDER_TRACK).height(SLIDER_TRACK).radius(radius).background(c.primary).absolute(Some(0), Some(top), None, None),
    ])
    .height(SLIDER_HEIGHT)
    .on_drag(on_drag)
}

pub fn menu_item<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, on_tap: M) -> Element<'a, M> {
    let theme = ui.theme();
    ui.row([ui.text(theme.fonts.body, label).grow(1)])
        .look(theme.looks.menu_item)
        .height(theme.metrics.touch_size + 8)
        .padding_x(theme.metrics.padding)
        .align(Align::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

/// A panel near the top right, closing on a tap outside.
pub fn menu<'a, M: Copy>(ui: &'a Ui<M>, top: i16, items: impl Children<'a, M>, on_close: M) -> Element<'a, M> {
    let theme = ui.theme();
    ui.column([
        ui.column([]).absolute(Some(0), Some(0), Some(0), Some(0)).id("menu-backdrop").on_tap(on_close),
        ui.column(items).width(MENU_WIDTH).padding_y(theme.metrics.gap).radius(20).background(theme.colors.surface).shadow(theme.menu_shadow()).absolute(None, Some(top), Some(16), None),
    ])
    .absolute(Some(0), Some(0), Some(0), Some(0))
}

/// A dialog over a scrim, closing on a tap outside.
pub fn modal<'a, M: Copy>(ui: &'a Ui<M>, content: Element<'a, M>, on_close: M) -> Element<'a, M> {
    let theme = ui.theme();
    let (width, height) = ui.screen_size();
    let left = (width - MODAL_WIDTH) / 2;
    ui.column([
        ui.column([]).absolute(Some(0), Some(0), Some(0), Some(0)).background(theme.colors.scrim).id("modal-scrim").on_tap(on_close),
        ui.column([content]).width(MODAL_WIDTH).padding(theme.metrics.padding).radius(20).background(theme.colors.surface).absolute(Some(left), Some(height / 3), None, None),
    ])
    .absolute(Some(0), Some(0), Some(0), Some(0))
}

pub fn toast<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.row([ui.text(theme.fonts.caption, text).color(theme.colors.surface)])
        .padding_x(theme.metrics.padding)
        .padding_y(theme.metrics.gap)
        .radius(theme.metrics.radius)
        .background(theme.colors.on_surface)
        .absolute(None, None, Some(theme.metrics.padding), Some(theme.metrics.padding))
}
