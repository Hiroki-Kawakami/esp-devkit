/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Widgets are functions building elements from the theme. State stays with the
 * caller, which passes it in and hears about changes through messages.
 */

use alloc::string::String;
use alloc::vec::Vec;

use devkit_gfx::Rect;

use crate::app::Handle;
use crate::component::{Component, Cx};
use crate::element::{Children, Drag, Element, Ui};
use crate::keyboard::{Keyboard, KeyboardPanel, KeyboardProps};
use crate::text_buffer::{TextBuffer, TextInput};
use crate::style::{Align, Justify, Role};
pub use crate::theme::DEFAULT;
use crate::theme::{Icon, WidgetLook};

const SWITCH_WIDTH: i16 = 64;
const SWITCH_HEIGHT: i16 = 36;
const SWITCH_THUMB: i16 = 28;
const CHECK_SIZE: i16 = 28;
const SLIDER_HEIGHT: i16 = 48;
const SLIDER_TRACK: i16 = 8;
const SLIDER_THUMB: i16 = 28;
const SEGMENT_HEIGHT: i16 = 56;
const SEGMENT_INSET: i16 = 4;
const SEGMENT_PADDING: i16 = 20;
const DROPDOWN_WIDTH: i16 = 260;
const TABLE_RADIUS: u8 = 24;
const TABLE_ROW_HEIGHT: i16 = 88;
const TABLE_HEADER_INDENT: i16 = 8;
const TABLE_HEADER_TOP: i16 = 32;
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

pub fn icon_button<'a, M: Copy>(ui: &'a Ui<M>, icon: Icon, on_tap: M) -> Element<'a, M> {
    let theme = ui.theme();
    let size = theme.metrics.touch_size;
    ui.row([ui.icon(icon)])
        .look(theme.looks.icon_button)
        .size(size, size)
        .radius((size / 2) as u8)
        .align(Align::Center)
        .justify(Justify::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

pub fn list_item<'a, M: Copy>(ui: &'a Ui<M>, leading: Option<Icon>, title: &'a str, meta: Option<&'a str>, trailing: Option<Icon>, on_tap: M) -> Element<'a, M> {
    let theme = ui.theme();
    let (c, m) = (ui.colors(), &theme.metrics);
    let lead = leading.map_or(ui.empty(), |icon| ui.icon(icon).color(c.muted));
    let tail = trailing.map_or(ui.empty(), |icon| ui.icon(icon).color(c.divider));
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

/// `back` makes the back icon and the title one button; `actions` follow it.
pub fn top_bar<'a, M: Copy + 'static>(ui: &'a Ui<M>, back: Option<M>, title: &'a str, actions: Element<'a, M>) -> Element<'a, M> {
    let theme = ui.theme();
    let m = &theme.metrics;
    let label = ui.text(theme.fonts.title, title).ellipsis();
    let lead = match back {
        Some(msg) => {
            let left = (m.touch_size - theme.icons.back.font.px() as i16) / 2;
            let right = (m.touch_size - theme.fonts.title.px() as i16) / 2;
            let button = ui
                .row([ui.icon(theme.icons.back).shrink(0), label])
                .look(theme.looks.icon_button)
                .height(m.touch_size)
                .padding_each(left, 0, right, 0)
                .shrink(1)
                .radius(m.radius)
                .gap(m.gap)
                .align(Align::Center)
                .role(Role::Button)
                .on_tap(msg)
                .id("back");
            ui.row([button]).grow(1)
        }
        None => label.padding_x(m.padding - m.bar_padding).grow(1),
    };
    ui.row([lead, actions])
        .height(m.bar_height)
        .padding_x(m.bar_padding)
        .gap(m.gap)
        .align(Align::Center)
        .role(Role::Header)
}

pub fn switch<'a, M: Copy>(ui: &'a Ui<M>, on: bool, on_tap: M) -> Element<'a, M> {
    let c = ui.colors();
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
    let c = ui.colors();
    let mark = if checked { ui.column([]).size(CHECK_SIZE - 12, CHECK_SIZE - 12).radius(4).background(c.on_primary).absolute(Some(6), Some(6), None, None) } else { ui.empty() };
    ui.column([mark]).size(CHECK_SIZE, CHECK_SIZE).radius(6).background(if checked { c.primary } else { c.divider }).checked(checked).role(Role::Button).on_tap(on_tap)
}

/// Where a drag lands on a slider, 0..=255.
pub fn slider_value(drag: Drag) -> u8 {
    let width = i32::from(drag.width - SLIDER_THUMB).max(1);
    (i32::from(drag.x - SLIDER_THUMB / 2).clamp(0, width) * i32::from(VALUE_RANGE) / width) as u8
}

/// `value` is 0..=255. Takes the width it is given.
pub fn slider<'a, M: Copy>(ui: &'a Ui<M>, value: u8, on_drag: fn(Drag) -> M) -> Element<'a, M> {
    let c = ui.colors();
    let top = (SLIDER_HEIGHT - SLIDER_TRACK) / 2;
    let radius = (SLIDER_TRACK / 2) as u8;
    let thumb = ui.column([]).size(SLIDER_THUMB, SLIDER_THUMB).radius((SLIDER_THUMB / 2) as u8).background(c.primary).absolute(None, Some((SLIDER_TRACK - SLIDER_THUMB) / 2), Some(-SLIDER_THUMB / 2), None);
    ui.column([ui.column([
        ui.column([]).height(SLIDER_TRACK).radius(radius).background(c.selected).absolute(Some(0), Some(top), Some(0), None),
        ui.column([thumb]).width_fraction(u16::from(value), VALUE_RANGE).min_width(SLIDER_TRACK).height(SLIDER_TRACK).radius(radius).background(c.primary).absolute(Some(0), Some(top), None, None),
    ])
    .height(SLIDER_HEIGHT)])
    .padding_x(SLIDER_THUMB / 2)
    .on_drag(on_drag)
}

/// One choice out of `options`, which pair a label with what tapping it sends.
pub fn segmented<'a, M: Copy>(ui: &'a Ui<M>, options: &[(&'a str, M)], selected: usize) -> Element<'a, M> {
    let theme = ui.theme();
    let m = &theme.metrics;
    let segments: Vec<_> = options
        .iter()
        .enumerate()
        .map(|(i, &(label, on_tap))| {
            ui.row([ui.text(theme.fonts.caption, label)])
                .look(theme.looks.segment)
                .height(SEGMENT_HEIGHT - SEGMENT_INSET * 2)
                .padding_x(SEGMENT_PADDING)
                .radius(m.radius - SEGMENT_INSET as u8)
                .align(Align::Center)
                .justify(Justify::Center)
                .selected(i == selected)
                .role(Role::Button)
                .on_tap(on_tap)
        })
        .collect();
    ui.row(segments).padding(SEGMENT_INSET).radius(m.radius).background(ui.colors().background)
}

fn dropdown_row<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, trailing: Element<'a, M>) -> Element<'a, M> {
    let theme = ui.theme();
    let m = &theme.metrics;
    ui.row([ui.text(theme.fonts.caption, label).ellipsis().grow(1), trailing]).height(SEGMENT_HEIGHT).padding_x(m.gap).gap(m.gap).align(Align::Center).role(Role::Button)
}

/// Shows `label`; `on_open` says where it is, for a `DropdownMenu`.
pub fn dropdown<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, on_open: fn(Rect) -> M) -> Element<'a, M> {
    let theme = ui.theme();
    dropdown_row(ui, label, ui.icon(theme.icons.expand).color(ui.colors().muted).shrink(0)).look(theme.looks.button_secondary).width(DROPDOWN_WIDTH).radius(theme.metrics.radius).on_tap_at(on_open)
}

/// A popup touching `anchor`: under it, or above it where it would not fit.
/// Sends `on_select` of the chosen option to `target`, and closes then or on a
/// tap outside.
pub struct DropdownMenu<M: Copy + 'static> {
    anchor: Rect,
    options: Vec<String>,
    selected: usize,
    target: Handle<M>,
    on_select: fn(usize) -> M,
}

impl<M: Copy + 'static> DropdownMenu<M> {
    pub fn new(anchor: Rect, options: &[&str], selected: usize, target: Handle<M>, on_select: fn(usize) -> M) -> Self {
        Self { anchor, options: options.iter().map(|option| String::from(*option)).collect(), selected, target, on_select }
    }
}

impl<M: Copy + 'static> Component for DropdownMenu<M> {
    type Msg = Option<usize>;

    fn update(&mut self, msg: Option<usize>, cx: &mut Cx<Self>) {
        if let Some(index) = msg {
            cx.send_to(&self.target, (self.on_select)(index));
        }
        cx.close_popup();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Option<usize>>) -> Element<'a, Option<usize>> {
        let theme = ui.theme();
        let anchor = self.anchor;
        let height = self.options.len() as i16 * SEGMENT_HEIGHT;
        let top = if anchor.bottom() + height > ui.screen_size().1 { anchor.y - height } else { anchor.bottom() };
        let rows: Vec<_> = self.options.iter().enumerate().map(|(i, label)| dropdown_row(ui, label, ui.empty()).look(theme.looks.dropdown_item).selected(i == self.selected).on_tap(Some(i))).collect();
        ui.column([
            ui.column([]).absolute(Some(0), Some(0), Some(0), Some(0)).id("menu-backdrop").on_tap(None),
            ui.column(rows).width(anchor.w).look(theme.looks.button_secondary).radius(theme.metrics.radius).clip().absolute(Some(anchor.x), Some(top), None, None).id("dropdown-menu"),
        ])
    }
}

/// A scroller with a table's margins. Takes the height it is given.
pub fn table<'a, M: Copy>(ui: &'a Ui<M>, children: impl Children<'a, M>) -> Element<'a, M> {
    let padding = ui.theme().metrics.padding;
    ui.scroll(ui.column(children).padding_each(padding, 0, padding, padding + ui.insets().bottom))
}

/// The title above a `table_section`.
pub fn table_header<'a, M: Copy>(ui: &'a Ui<M>, title: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.caption, title).color(ui.colors().muted).padding_each(TABLE_HEADER_INDENT, TABLE_HEADER_TOP, TABLE_HEADER_INDENT, theme.metrics.gap)
}

/// Rows in one rounded panel, with a divider between them.
pub fn table_section<'a, M: Copy>(ui: &'a Ui<M>, rows: impl IntoIterator<Item = Element<'a, M>>) -> Element<'a, M> {
    let padding = ui.theme().metrics.padding;
    let c = ui.colors();
    let mut children = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            children.push(ui.column([ui.column([]).height(1).background(c.divider)]).padding_x(padding));
        }
        children.push(row);
    }
    ui.column(children).radius(TABLE_RADIUS).background(c.surface).clip()
}

/// A row of `children`, as tall as they need above a minimum. Shows presses
/// once it has `on_tap`.
pub fn table_row<'a, M: Copy>(ui: &'a Ui<M>, children: impl Children<'a, M>) -> Element<'a, M> {
    let theme = ui.theme();
    let m = &theme.metrics;
    ui.row(children).look(theme.looks.menu_item).min_height(TABLE_ROW_HEIGHT).padding_x(m.padding).gap(m.gap).align(Align::Center)
}

/// Text taking the room left in a `table_row`, which pushes what follows to the end.
pub fn table_label<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    ui.text(ui.theme().fonts.body, text).ellipsis().grow(1)
}

pub fn table_value<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    ui.text(ui.theme().fonts.body, text).color(ui.colors().muted)
}

pub fn table_chevron<'a, M: Copy>(ui: &'a Ui<M>) -> Element<'a, M> {
    ui.icon(ui.theme().icons.chevron).color(ui.colors().muted).shrink(0)
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
        ui.column(items).width(MENU_WIDTH).padding_y(theme.metrics.gap).radius(20).background(ui.colors().surface).shadow(ui.colors().menu_shadow()).absolute(None, Some(top), Some(16), None),
    ])
    .absolute(Some(0), Some(0), Some(0), Some(0))
}

/// A dialog over a scrim, closing on a tap outside.
pub fn modal<'a, M: Copy>(ui: &'a Ui<M>, content: Element<'a, M>, on_close: M) -> Element<'a, M> {
    let theme = ui.theme();
    let (width, height) = ui.screen_size();
    let left = (width - MODAL_WIDTH) / 2;
    ui.column([
        ui.column([]).absolute(Some(0), Some(0), Some(0), Some(0)).background(ui.colors().scrim).id("modal-scrim").on_tap(on_close),
        ui.column([content]).width(MODAL_WIDTH).padding(theme.metrics.padding).radius(20).background(ui.colors().surface).absolute(Some(left), Some(height / 3), None, None),
    ])
    .absolute(Some(0), Some(0), Some(0), Some(0))
}

pub fn toast<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.row([ui.text(theme.fonts.caption, text).color(ui.colors().surface)])
        .padding_x(theme.metrics.padding)
        .padding_y(theme.metrics.gap)
        .radius(theme.metrics.radius)
        .background(ui.colors().on_surface)
        .absolute(None, None, Some(theme.metrics.padding), Some(theme.metrics.padding))
}

fn editable<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str, cursor: usize, underline: Option<(usize, usize)>, on_input: fn(TextInput) -> M) -> Element<'a, M> {
    let theme = ui.theme();
    let element = ui.text(theme.fonts.body, text).edit(cursor, underline);
    let element = if text.is_empty() { element.color(ui.colors().muted) } else { element };
    element
        .look(theme.looks.text_field)
        .radius(theme.metrics.radius)
        .padding_x(theme.metrics.gap)
        .padding_y(theme.metrics.gap)
        .role(Role::TextField)
        .on_input(on_input)
}

/// One line of text. `buffer` belongs to the caller, which applies what
/// `on_input` reports.
pub fn text_field<'a, M: Copy>(ui: &'a Ui<M>, buffer: &'a TextBuffer, on_input: fn(TextInput) -> M) -> Element<'a, M> {
    editable(ui, buffer.as_str(), buffer.cursor(), buffer.composing(), on_input)
}

/// A field showing one mark per byte, so tap positions stay byte offsets.
pub fn password_field<'a, M: Copy>(ui: &'a Ui<M>, buffer: &'a TextBuffer, on_input: fn(TextInput) -> M) -> Element<'a, M> {
    let marks = "*".repeat(buffer.as_str().len());
    editable(ui, ui.store(&marks), buffer.cursor(), None, on_input)
}

/// Text over several lines, wrapping at the width it is given.
pub fn text_area<'a, M: Copy>(ui: &'a Ui<M>, buffer: &'a TextBuffer, on_input: fn(TextInput) -> M) -> Element<'a, M> {
    text_field(ui, buffer, on_input).wrap().min_height(ui.theme().metrics.row_height * 2)
}

/// A keyboard placed by the view, for inputs that keep one on screen. Its keys
/// type into the focused input, as the docked one does.
pub fn keyboard<'a, M: Copy + 'static>(ui: &'a Ui<M>, keyboard: &'static Keyboard, width: i16) -> Element<'a, M> {
    ui.child::<KeyboardPanel>(KeyboardProps { keyboard, width })
}
