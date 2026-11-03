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
use crate::presence::Presence;
use crate::progress::Gauge;
use crate::keyboard::{Keyboard, KeyboardPanel, KeyboardProps};
use crate::text_buffer::{TextBuffer, TextInput};
use crate::style::{Align, Justify, Part, Role, State, StateStyle};
use crate::theme::Icon;

const VALUE_RANGE: u16 = 255;
const PART_STATES: State = State::from_bits_retain(State::PRESSED.bits() | State::CHECKED.bits() | State::DISABLED.bits());

/// `style` is usually a field of `ui.theme().widgets.button`.
pub fn button<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, style: &'static StateStyle, on_tap: M) -> Element<'a, M> {
    let theme = ui.theme();
    ui.row([ui.text(theme.fonts.body, label).style(theme.widgets.button.label)])
        .style(style)
        .align(Align::Center)
        .justify(Justify::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

pub fn icon_button<'a, M: Copy>(ui: &'a Ui<M>, icon: Icon, on_tap: M) -> Element<'a, M> {
    ui.row([ui.icon(icon)])
        .style(ui.theme().widgets.icon_button.root)
        .align(Align::Center)
        .justify(Justify::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

pub fn list_item<'a, M: Copy>(ui: &'a Ui<M>, leading: Option<Icon>, title: &'a str, meta: Option<&'a str>, trailing: Option<Icon>, on_tap: M) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.list_item;
    let lead = leading.map_or(ui.empty(), |icon| ui.icon(icon).style(t.leading).part_of(Part::Leading));
    let tail = trailing.map_or(ui.empty(), |icon| ui.icon(icon).style(t.trailing).part_of(Part::Trailing));
    let detail = meta.map_or(ui.empty(), |text| ui.text(theme.fonts.caption, text).style(t.meta).part_of(Part::Meta));
    ui.row([lead, ui.column([ui.text(theme.fonts.body, title).style(t.title).ellipsis().part_of(Part::Title), detail]).style(t.text).grow(1), tail])
        .style(t.root)
        .align(Align::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

/// `back` makes the back icon and the title one button; `actions` follow it.
pub fn top_bar<'a, M: Copy + 'static>(ui: &'a Ui<M>, back: Option<M>, title: &'a str, actions: Element<'a, M>) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.top_bar;
    let label = ui.text(theme.fonts.title, title).style(t.title).ellipsis().part_of(Part::Title);
    let lead = match back {
        Some(msg) => {
            let left = (t.back_height - theme.icons.back.font.px() as i16) / 2;
            let right = (t.back_height - label.font_px()) / 2;
            let button = ui
                .row([ui.icon(theme.icons.back).shrink(0), label])
                .style(t.back)
                .padding_each(left, 0, right, 0)
                .shrink(1)
                .align(Align::Center)
                .role(Role::Button)
                .on_tap(msg)
                .id("back")
                .part_of(Part::Back);
            ui.row([button]).grow(1)
        }
        None => label.padding_x(theme.widgets.unit.padding - t.padding_x).grow(1),
    };
    ui.row([lead, actions]).style(t.root).align(Align::Center).role(Role::Header)
}

pub fn switch<'a, M: Copy>(ui: &'a Ui<M>, on: bool, on_tap: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.switch;
    ui.row([ui.column([]).style(t.thumb).inherit_state(PART_STATES).part_of(Part::Thumb)])
        .style(t.root)
        .align(Align::Center)
        .justify(if on { Justify::End } else { Justify::Start })
        .checked(on)
        .state_source()
        .part_of(Part::Track)
        .role(Role::Button)
        .on_tap(on_tap)
}

pub fn checkbox<'a, M: Copy>(ui: &'a Ui<M>, checked: bool, on_tap: M) -> Element<'a, M> {
    let t = &ui.theme().widgets.checkbox;
    let mark = if checked { ui.column([]).style(t.mark).inherit_state(PART_STATES).part_of(Part::Mark) } else { ui.empty() };
    ui.row([mark]).style(t.root).align(Align::Center).justify(Justify::Center).checked(checked).state_source().role(Role::Button).on_tap(on_tap)
}

/// Where a drag lands on a slider, 0..=255.
pub fn slider_value(drag: Drag) -> u8 {
    let width = i32::from(drag.width).max(1);
    (i32::from(drag.x).clamp(0, width) * i32::from(VALUE_RANGE) / width) as u8
}

/// `value` is 0..=255. Takes the width it is given.
pub fn slider<'a, M: Copy>(ui: &'a Ui<M>, value: u8, on_drag: fn(Drag) -> M) -> Element<'a, M> {
    let t = &ui.theme().widgets.slider;
    let overhang = t.inset - t.track_inset;
    let thumb = ui.column([]).style(t.thumb).inherit_state(PART_STATES).part_of(Part::Thumb);
    // No width, so the thumb stays centered on the end of the fill at any size.
    let end = ui.column([thumb]).width(0).align(Align::Center).justify(Justify::Center).absolute(None, Some(0), Some(0), Some(0));
    let fill = ui.column([end]).style(t.fill).absolute(Some(-overhang), Some(0), Some(0), Some(0)).inherit_state(PART_STATES).part_of(Part::Fill);
    let stop = ui.column([fill]).width_fraction(u16::from(value), VALUE_RANGE);
    let track = ui.row([stop]).style(t.track).padding_x(overhang).inherit_state(PART_STATES).part_of(Part::Track);
    // The track reaches past the root's padding, which the thumb and a drag keep to.
    let bar = ui.column([track]).padding_x(t.track_inset).justify(Justify::Center).absolute(Some(0), Some(0), Some(0), Some(0));
    ui.column([bar]).style(t.root).state_source().on_drag(on_drag)
}

/// `value` is 0.0..=1.0, `None` while how far is not known. Takes the width
/// it is given.
pub fn progress_bar<'a, M: Copy>(ui: &'a Ui<M>, value: Option<f32>) -> Element<'a, M> {
    let t = &ui.theme().widgets.progress_bar;
    let fill = ui.progress(Gauge::Bar { segment: t.segment }, value, t.period_ms).style(t.fill).absolute(Some(0), Some(0), Some(0), Some(0)).part_of(Part::Fill);
    ui.column([fill]).style(t.track).part_of(Part::Track)
}

/// `value` is 0.0..=1.0, `None` while how far is not known.
pub fn progress_indicator<'a, M: Copy>(ui: &'a Ui<M>, value: Option<f32>) -> Element<'a, M> {
    let t = &ui.theme().widgets.progress_indicator;
    let gauge = Gauge::Ring { stroke: t.stroke, sweep: t.sweep, track: t.track_color, fill: t.fill_color };
    ui.progress(gauge, value, t.period_ms).style(t.root)
}

/// One choice out of `options`, which pair a label with what tapping it sends.
pub fn segmented<'a, M: Copy>(ui: &'a Ui<M>, options: &[(&'a str, M)], selected: usize) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.segmented;
    let segments: Vec<_> = options
        .iter()
        .enumerate()
        .map(|(i, &(label, on_tap))| {
            ui.row([ui.text(theme.fonts.caption, label).style(t.label).part_of(Part::Label)])
                .style(t.item)
                .align(Align::Center)
                .justify(Justify::Center)
                .selected(i == selected)
                .role(Role::Button)
                .on_tap(on_tap)
                .part_of(Part::Item)
        })
        .collect();
    ui.row(segments).style(t.root)
}

fn dropdown_row<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, style: &'static StateStyle, trailing: Element<'a, M>) -> Element<'a, M> {
    let theme = ui.theme();
    ui.row([ui.text(theme.fonts.caption, label).style(theme.widgets.dropdown.label).ellipsis().grow(1), trailing]).style(style).align(Align::Center).role(Role::Button)
}

/// Shows `label`; `on_open` is the tap opening a `DropdownMenu`.
pub fn dropdown<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, on_open: M) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.dropdown;
    dropdown_row(ui, label, t.root, ui.icon(theme.icons.expand).style(t.icon).shrink(0)).on_tap(on_open)
}

/// `panel`, of `size`, touching `anchor`: under it or else above it, from its
/// left edge or else to its right one.
fn anchored<'a, M: Copy>(ui: &'a Ui<M>, anchor: Rect, (width, height): (i16, i16), panel: Element<'a, M>) -> Element<'a, M> {
    let (screen_w, screen_h) = ui.screen_size();
    let left = if anchor.x + width > screen_w { anchor.right() - width } else { anchor.x };
    let top = if anchor.bottom() + height > screen_h { anchor.y - height } else { anchor.bottom() };
    ui.column([panel.absolute(Some(left.min(screen_w - width).max(0)), Some(top.min(screen_h - height).max(0)), None, None)])
}

/// A popup by `anchor`, as wide as it.
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
    type Msg = usize;

    fn update(&mut self, index: usize, cx: &mut Cx<Self>) {
        cx.send_to(&self.target, (self.on_select)(index));
        cx.close_popup();
    }

    fn view<'a>(&'a self, ui: &'a Ui<usize>) -> Element<'a, usize> {
        let widgets = &ui.theme().widgets;
        let t = &widgets.dropdown_menu;
        let height = self.options.len() as i16 * widgets.dropdown.height;
        let rows: Vec<_> = self.options.iter().enumerate().map(|(i, label)| dropdown_row(ui, label, t.item, ui.empty()).selected(i == self.selected).on_tap(i)).collect();
        anchored(ui, self.anchor, (self.anchor.w, height), ui.column(rows).style(t.root).width(self.anchor.w).clip().id("dropdown-menu"))
    }
}

/// A scroller with a table's margins. Takes the height it is given.
pub fn table<'a, M: Copy>(ui: &'a Ui<M>, children: impl Children<'a, M>) -> Element<'a, M> {
    let padding = ui.theme().widgets.table.padding;
    ui.scroll(ui.column(children).padding_each(padding, 0, padding, padding + ui.insets().bottom))
}

/// The title above a `table_section`.
pub fn table_header<'a, M: Copy>(ui: &'a Ui<M>, title: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.caption, title).style(theme.widgets.table_header.root)
}

/// Rows in one rounded panel, with a divider between them.
pub fn table_section<'a, M: Copy>(ui: &'a Ui<M>, rows: impl IntoIterator<Item = Element<'a, M>>) -> Element<'a, M> {
    let t = &ui.theme().widgets.table_section;
    let mut children = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            children.push(ui.column([ui.column([]).style(t.line)]).style(t.divider));
        }
        children.push(row);
    }
    ui.column(children).style(t.root).clip()
}

/// A row of `children`, as tall as they need above a minimum. Shows presses
/// once it has `on_tap`.
pub fn table_row<'a, M: Copy>(ui: &'a Ui<M>, children: impl Children<'a, M>) -> Element<'a, M> {
    ui.row(children).style(ui.theme().widgets.table_row.root).align(Align::Center)
}

/// Text taking the room left in a `table_row`, which pushes what follows to the end.
pub fn table_label<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.body, text).style(theme.widgets.table_row.label).ellipsis().grow(1)
}

pub fn table_value<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.body, text).style(theme.widgets.table_row.value)
}

pub fn table_chevron<'a, M: Copy>(ui: &'a Ui<M>) -> Element<'a, M> {
    let theme = ui.theme();
    ui.icon(theme.icons.chevron).style(theme.widgets.table_row.chevron).shrink(0)
}

fn menu_item<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, on_tap: M) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.menu_item;
    ui.row([ui.text(theme.fonts.body, label).style(t.label).grow(1)])
        .style(t.root)
        .align(Align::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

/// A popup by `anchor` of `items`, which pair a label with what tapping it sends
/// to `target`. Closes then or on a tap outside.
pub struct Menu<M: Copy + 'static> {
    anchor: Rect,
    items: Vec<(String, M)>,
    target: Handle<M>,
}

impl<M: Copy + 'static> Menu<M> {
    pub fn new(anchor: Rect, items: &[(&str, M)], target: Handle<M>) -> Self {
        Self { anchor, items: items.iter().map(|&(label, msg)| (String::from(label), msg)).collect(), target }
    }
}

impl<M: Copy + 'static> Component for Menu<M> {
    type Msg = M;

    fn update(&mut self, msg: M, cx: &mut Cx<Self>) {
        cx.send_to(&self.target, msg);
        cx.close_popup();
    }

    fn view<'a>(&'a self, ui: &'a Ui<M>) -> Element<'a, M> {
        let widgets = &ui.theme().widgets;
        let t = &widgets.menu;
        let height = self.items.len() as i16 * widgets.menu_item.height + 2 * t.padding_y;
        let rows: Vec<_> = self.items.iter().map(|(label, msg)| menu_item(ui, label, *msg)).collect();
        anchored(ui, self.anchor, (t.width, height), ui.column(rows).style(t.panel).part_of(Part::Panel).id("menu"))
    }
}

/// A dialog for a screen on the `Overlay`.
pub fn modal<'a, M: Copy>(ui: &'a Ui<M>, content: Element<'a, M>) -> Element<'a, M> {
    let t = &ui.theme().widgets.modal;
    let (width, height) = ui.screen_size();
    let left = (width - t.width) / 2;
    ui.column([ui
        .column([content])
        .style(t.panel)
        .width(t.width)
        .absolute(Some(left), Some(height / 3), None, None)
        .appear(Presence::fade().offset(0, 24).ms(150))
        .disappear(Presence::fade().ms(100))
        .part_of(Part::Panel)])
    .absolute(Some(0), Some(0), Some(0), Some(0))
}

pub fn toast<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.toast;
    ui.row([ui.text(theme.fonts.caption, text).style(t.label)]).style(t.root)
}

fn editable<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str, cursor: usize, underline: Option<(usize, usize)>, on_input: fn(TextInput) -> M) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.text_field;
    let element = ui.text(theme.fonts.body, text).edit(cursor, underline).style(t.root);
    let element = if text.is_empty() { element.foreground(t.placeholder_color) } else { element };
    element.role(Role::TextField).on_input(on_input)
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
