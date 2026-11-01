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
use crate::style::{Align, Justify, Role, Shadow};
pub use crate::theme::DEFAULT;
use crate::theme::{Icon, WidgetLook};

const VALUE_RANGE: u16 = 255;

/// `look` is usually `ui.theme().widgets.button.primary` or `.secondary`.
pub fn button<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, look: &'static WidgetLook, on_tap: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.button;
    ui.row([ui.text(t.font, label)])
        .look(look)
        .height(t.height)
        .padding_x(t.padding_x)
        .radius(t.radius)
        .align(Align::Center)
        .justify(Justify::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

pub fn icon_button<'a, M: Copy>(ui: &'a Ui<M>, icon: Icon, on_tap: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.icon_button;
    ui.row([ui.icon(icon)])
        .look(t.look)
        .size(t.size, t.size)
        .radius(t.radius)
        .align(Align::Center)
        .justify(Justify::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

pub fn list_item<'a, M: Copy>(ui: &'a Ui<M>, leading: Option<Icon>, title: &'a str, meta: Option<&'a str>, trailing: Option<Icon>, on_tap: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.list_item;
    let lead = leading.map_or(ui.empty(), |icon| ui.icon(icon).color(ui.color(t.leading_color)));
    let tail = trailing.map_or(ui.empty(), |icon| ui.icon(icon).color(ui.color(t.trailing_color)));
    let detail = meta.map_or(ui.empty(), |text| ui.text(t.meta_font, text).color(ui.color(t.meta_color)));
    ui.row([lead, ui.column([ui.text(t.title_font, title).ellipsis(), detail]).gap(t.text_gap).grow(1), tail])
        .look(t.look)
        .height(t.height)
        .padding_x(t.padding_x)
        .gap(t.gap)
        .align(Align::Center)
        .border_bottom(ui.color(t.divider_color))
        .role(Role::Button)
        .on_tap(on_tap)
}

/// `back` makes the back icon and the title one button; `actions` follow it.
pub fn top_bar<'a, M: Copy + 'static>(ui: &'a Ui<M>, back: Option<M>, title: &'a str, actions: Element<'a, M>) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.top_bar;
    let label = ui.text(t.font, title).ellipsis();
    let lead = match back {
        Some(msg) => {
            let left = (t.back_height - theme.icons.back.font.px() as i16) / 2;
            let right = (t.back_height - t.font.px() as i16) / 2;
            let button = ui
                .row([ui.icon(theme.icons.back).shrink(0), label])
                .look(t.back_look)
                .height(t.back_height)
                .padding_each(left, 0, right, 0)
                .shrink(1)
                .radius(t.back_radius)
                .gap(t.gap)
                .align(Align::Center)
                .role(Role::Button)
                .on_tap(msg)
                .id("back");
            ui.row([button]).grow(1)
        }
        None => label.padding_x(theme.widgets.unit.padding - t.padding_x).grow(1),
    };
    ui.row([lead, actions])
        .height(t.height)
        .padding_x(t.padding_x)
        .gap(t.gap)
        .align(Align::Center)
        .role(Role::Header)
}

pub fn switch<'a, M: Copy>(ui: &'a Ui<M>, on: bool, on_tap: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.switch;
    let inset = (t.height - t.thumb) / 2;
    let x = if on { t.width - t.thumb - inset } else { inset };
    ui.column([ui.column([]).size(t.thumb, t.thumb).radius((t.thumb / 2) as u8).background(ui.color(t.thumb_color)).absolute(Some(x), Some(inset), None, None)])
        .size(t.width, t.height)
        .radius((t.height / 2) as u8)
        .background(ui.color(if on { t.on_color } else { t.off_color }))
        .checked(on)
        .role(Role::Button)
        .on_tap(on_tap)
}

pub fn checkbox<'a, M: Copy>(ui: &'a Ui<M>, checked: bool, on_tap: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.checkbox;
    let mark_size = t.size - t.mark_inset * 2;
    let mark = if checked { ui.column([]).size(mark_size, mark_size).radius(t.mark_radius).background(ui.color(t.mark_color)).absolute(Some(t.mark_inset), Some(t.mark_inset), None, None) } else { ui.empty() };
    ui.column([mark]).size(t.size, t.size).radius(t.radius).background(ui.color(if checked { t.on_color } else { t.off_color })).checked(checked).role(Role::Button).on_tap(on_tap)
}

/// Where a drag lands on a slider, 0..=255.
pub fn slider_value(drag: Drag) -> u8 {
    let width = i32::from(drag.width).max(1);
    (i32::from(drag.x).clamp(0, width) * i32::from(VALUE_RANGE) / width) as u8
}

/// `value` is 0..=255. Takes the width it is given.
pub fn slider<'a, M: Copy>(ui: &'a Ui<M>, value: u8, on_drag: fn(Drag) -> M) -> Element<'a, M> {
    let t = &ui.theme().widgets.slider;
    let top = (t.height - t.track) / 2;
    let radius = (t.track / 2) as u8;
    let thumb = ui.column([]).size(t.thumb, t.thumb).radius((t.thumb / 2) as u8).background(ui.color(t.thumb_color)).absolute(None, Some((t.track - t.thumb) / 2), Some(-t.thumb / 2), None);
    ui.column([ui.column([
        ui.column([]).height(t.track).radius(radius).background(ui.color(t.track_color)).absolute(Some(0), Some(top), Some(0), None),
        ui.column([thumb]).width_fraction(u16::from(value), VALUE_RANGE).min_width(t.track).height(t.track).radius(radius).background(ui.color(t.active_color)).absolute(Some(0), Some(top), None, None),
    ])
    .height(t.height)])
    .padding_x(t.thumb / 2)
    .on_drag(on_drag)
}

/// One choice out of `options`, which pair a label with what tapping it sends.
pub fn segmented<'a, M: Copy>(ui: &'a Ui<M>, options: &[(&'a str, M)], selected: usize) -> Element<'a, M> {
    let t = &ui.theme().widgets.segmented;
    let segments: Vec<_> = options
        .iter()
        .enumerate()
        .map(|(i, &(label, on_tap))| {
            ui.row([ui.text(t.font, label)])
                .look(t.look)
                .height(t.height - t.inset * 2)
                .padding_x(t.item_padding_x)
                .radius(t.item_radius)
                .align(Align::Center)
                .justify(Justify::Center)
                .selected(i == selected)
                .role(Role::Button)
                .on_tap(on_tap)
        })
        .collect();
    ui.row(segments).padding(t.inset).radius(t.radius).background(ui.color(t.background))
}

fn dropdown_row<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, trailing: Element<'a, M>) -> Element<'a, M> {
    let t = &ui.theme().widgets.dropdown;
    ui.row([ui.text(t.font, label).ellipsis().grow(1), trailing]).height(t.height).padding_x(t.padding_x).gap(t.gap).align(Align::Center).role(Role::Button)
}

/// Shows `label`; `on_open` says where it is, for a `DropdownMenu`.
pub fn dropdown<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, on_open: fn(Rect) -> M) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.dropdown;
    dropdown_row(ui, label, ui.icon(theme.icons.expand).color(ui.color(t.icon_color)).shrink(0)).look(t.look).width(t.width).radius(t.radius).on_tap_at(on_open)
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
        let widgets = &ui.theme().widgets;
        let t = &widgets.dropdown_menu;
        let anchor = self.anchor;
        let height = self.options.len() as i16 * widgets.dropdown.height;
        let top = if anchor.bottom() + height > ui.screen_size().1 { anchor.y - height } else { anchor.bottom() };
        let rows: Vec<_> = self.options.iter().enumerate().map(|(i, label)| dropdown_row(ui, label, ui.empty()).look(t.item_look).selected(i == self.selected).on_tap(Some(i))).collect();
        ui.column([
            ui.column([]).absolute(Some(0), Some(0), Some(0), Some(0)).id("menu-backdrop").on_tap(None),
            ui.column(rows).width(anchor.w).look(t.look).radius(t.radius).clip().absolute(Some(anchor.x), Some(top), None, None).id("dropdown-menu"),
        ])
    }
}

/// A scroller with a table's margins. Takes the height it is given.
pub fn table<'a, M: Copy>(ui: &'a Ui<M>, children: impl Children<'a, M>) -> Element<'a, M> {
    let padding = ui.theme().widgets.table.padding;
    ui.scroll(ui.column(children).padding_each(padding, 0, padding, padding + ui.insets().bottom))
}

/// The title above a `table_section`.
pub fn table_header<'a, M: Copy>(ui: &'a Ui<M>, title: &'a str) -> Element<'a, M> {
    let t = &ui.theme().widgets.table_header;
    ui.text(t.font, title).color(ui.color(t.color)).padding_each(t.indent, t.top, t.indent, t.bottom)
}

/// Rows in one rounded panel, with a divider between them.
pub fn table_section<'a, M: Copy>(ui: &'a Ui<M>, rows: impl IntoIterator<Item = Element<'a, M>>) -> Element<'a, M> {
    let t = &ui.theme().widgets.table_section;
    let mut children = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            children.push(ui.column([ui.column([]).height(1).background(ui.color(t.divider_color))]).padding_x(t.divider_inset));
        }
        children.push(row);
    }
    ui.column(children).radius(t.radius).background(ui.color(t.background)).clip()
}

/// A row of `children`, as tall as they need above a minimum. Shows presses
/// once it has `on_tap`.
pub fn table_row<'a, M: Copy>(ui: &'a Ui<M>, children: impl Children<'a, M>) -> Element<'a, M> {
    let t = &ui.theme().widgets.table_row;
    ui.row(children).look(t.look).min_height(t.min_height).padding_x(t.padding_x).gap(t.gap).align(Align::Center)
}

/// Text taking the room left in a `table_row`, which pushes what follows to the end.
pub fn table_label<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    ui.text(ui.theme().widgets.table_row.label_font, text).ellipsis().grow(1)
}

pub fn table_value<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let t = &ui.theme().widgets.table_row;
    ui.text(t.value_font, text).color(ui.color(t.value_color))
}

pub fn table_chevron<'a, M: Copy>(ui: &'a Ui<M>) -> Element<'a, M> {
    let theme = ui.theme();
    ui.icon(theme.icons.chevron).color(ui.color(theme.widgets.table_row.chevron_color)).shrink(0)
}

pub fn menu_item<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, on_tap: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.menu_item;
    ui.row([ui.text(t.font, label).grow(1)])
        .look(t.look)
        .height(t.height)
        .padding_x(t.padding_x)
        .align(Align::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

/// A panel near the top right, closing on a tap outside.
pub fn menu<'a, M: Copy>(ui: &'a Ui<M>, top: i16, items: impl Children<'a, M>, on_close: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.menu;
    let shadow = Shadow { y: t.shadow_y, blur: t.shadow_blur, color: ui.color(t.shadow_color) };
    ui.column([
        ui.column([]).absolute(Some(0), Some(0), Some(0), Some(0)).id("menu-backdrop").on_tap(on_close),
        ui.column(items).width(t.width).padding_y(t.padding_y).radius(t.radius).background(ui.color(t.background)).shadow(shadow).absolute(None, Some(top), Some(t.right), None),
    ])
    .absolute(Some(0), Some(0), Some(0), Some(0))
}

/// A dialog over a scrim, closing on a tap outside.
pub fn modal<'a, M: Copy>(ui: &'a Ui<M>, content: Element<'a, M>, on_close: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.modal;
    let (width, height) = ui.screen_size();
    let left = (width - t.width) / 2;
    ui.column([
        ui.column([]).absolute(Some(0), Some(0), Some(0), Some(0)).background(ui.color(t.scrim)).id("modal-scrim").on_tap(on_close),
        ui.column([content]).width(t.width).padding(t.padding).radius(t.radius).background(ui.color(t.background)).absolute(Some(left), Some(height / 3), None, None),
    ])
    .absolute(Some(0), Some(0), Some(0), Some(0))
}

pub fn toast<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let t = &ui.theme().widgets.toast;
    ui.row([ui.text(t.font, text).color(ui.color(t.foreground))])
        .padding_x(t.padding_x)
        .padding_y(t.padding_y)
        .radius(t.radius)
        .background(ui.color(t.background))
        .absolute(None, None, Some(t.margin), Some(t.margin))
}

fn editable<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str, cursor: usize, underline: Option<(usize, usize)>, on_input: fn(TextInput) -> M) -> Element<'a, M> {
    let t = &ui.theme().widgets.text_field;
    let element = ui.text(t.font, text).edit(cursor, underline);
    let element = if text.is_empty() { element.color(ui.color(t.placeholder_color)) } else { element };
    element
        .look(t.look)
        .radius(t.radius)
        .padding_x(t.padding_x)
        .padding_y(t.padding_y)
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
    text_field(ui, buffer, on_input).wrap().min_height(ui.theme().widgets.text_field.area_min_height)
}

/// A keyboard placed by the view, for inputs that keep one on screen. Its keys
/// type into the focused input, as the docked one does.
pub fn keyboard<'a, M: Copy + 'static>(ui: &'a Ui<M>, keyboard: &'static Keyboard, width: i16) -> Element<'a, M> {
    ui.child::<KeyboardPanel>(KeyboardProps { keyboard, width })
}
