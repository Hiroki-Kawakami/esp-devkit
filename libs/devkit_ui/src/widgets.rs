/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Widgets are functions building elements from the theme. State stays with the
 * caller, which passes it in and hears about changes through messages.
 */

use alloc::string::String;
use alloc::vec::Vec;

use devkit_gfx::{Elide, Rect};

use crate::app::Handle;
use crate::id;
use crate::component::{Component, Cx};
use crate::element::{Children, Drag, Element, Ui};
use crate::popover::{Placement, PopoverAlign, popover};
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
    button_from(ui.row([button_label(ui, label)]), style, on_tap)
}

pub fn button_from<'a, M: Copy>(element: Element<'a, M>, style: &'static StateStyle, on_tap: M) -> Element<'a, M> {
    element.style(style).align(Align::Center).justify(Justify::Center).role(Role::Button).on_tap(on_tap)
}

pub fn button_label<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.body, label).style(theme.widgets.button.label)
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
    ui.row([lead, ui.column([ui.text(theme.fonts.body, title).style(t.title).ellipsis(Elide::End).part_of(Part::Title), detail]).style(t.text).grow(1), tail])
        .style(t.root)
        .align(Align::Center)
        .role(Role::Button)
        .on_tap(on_tap)
}

/// `back` makes the back icon and the title one button; `actions` follow it.
pub fn top_bar<'a, M: Copy + 'static>(ui: &'a Ui<M>, back: Option<M>, title: &'a str, actions: Element<'a, M>) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.top_bar;
    let label = ui.text(theme.fonts.title, title).style(t.title).ellipsis(Elide::End).part_of(Part::Title);
    let lead = match back {
        Some(msg) => {
            let left = (t.back_height - t.back_icon.font.px() as i16) / 2;
            let right = (t.back_height - label.font_px()) / 2;
            let button = ui
                .row([ui.icon(t.back_icon).shrink(0), label])
                .style(t.back)
                .padding_each(left, 0, right, 0)
                .shrink(1)
                .align(Align::Center)
                .role(Role::Button)
                .on_tap(msg)
                .id(id!("back"))
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
    ui.row([ui.text(theme.fonts.caption, label).style(theme.widgets.dropdown.label).ellipsis(Elide::End).grow(1), trailing]).style(style).align(Align::Center).role(Role::Button)
}

/// Shows `label`; `on_open` is the tap opening a `DropdownMenu`.
pub fn dropdown<'a, M: Copy>(ui: &'a Ui<M>, label: &'a str, on_open: M) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.dropdown;
    dropdown_row(ui, label, t.root, ui.icon(t.expand_icon).style(t.icon).shrink(0)).on_tap(on_open)
}

/// `panel`, of `size`, touching `anchor`: under it or else above it, from its
/// left edge or else to its right one.
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
        let t = &ui.theme().widgets.dropdown_menu;
        let rows: Vec<_> = self.options.iter().enumerate().map(|(i, label)| dropdown_row(ui, label, t.item, ui.empty()).selected(i == self.selected).on_tap(i)).collect();
        let panel = ui.column([ui.scroll(ui.column(rows))]).style(t.root).width(self.anchor.w).clip().id(id!("dropdown-menu"));
        popover(ui, self.anchor, Placement::Auto, PopoverAlign::Start, panel)
    }
}

/// A line across the width it is given.
pub fn divider<'a, M: Copy>(ui: &'a Ui<M>) -> Element<'a, M> {
    let t = &ui.theme().widgets.divider;
    ui.column([ui.column([]).style(t.line).height(t.thickness).part_of(Part::Line)]).style(t.horizontal).align_self(Align::Stretch).shrink(0)
}

/// A line down the height it is given.
pub fn vertical_divider<'a, M: Copy>(ui: &'a Ui<M>) -> Element<'a, M> {
    let t = &ui.theme().widgets.divider;
    ui.row([ui.column([]).style(t.line).width(t.thickness).part_of(Part::Line)]).style(t.vertical).align_self(Align::Stretch).shrink(0)
}

/// A scroller with a table's margins.
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
    let widgets = &ui.theme().widgets;
    let mut children = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            children.push(divider(ui).padding_x(widgets.unit.padding));
        }
        children.push(row);
    }
    ui.column(children).style(widgets.table_section.root).clip()
}

/// A row of `children`, as tall as they need above a minimum. Shows presses
/// once it has `on_tap`.
pub fn table_row<'a, M: Copy>(ui: &'a Ui<M>, children: impl Children<'a, M>) -> Element<'a, M> {
    ui.row(children).style(ui.theme().widgets.table_row.root).align(Align::Center)
}

/// Text taking the room left in a `table_row`, which pushes what follows to the end.
pub fn table_label<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.body, text).style(theme.widgets.table_row.label).ellipsis(Elide::End).grow(1)
}

pub fn table_value<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.body, text).style(theme.widgets.table_row.value)
}

pub fn table_chevron<'a, M: Copy>(ui: &'a Ui<M>) -> Element<'a, M> {
    let t = &ui.theme().widgets.table_row;
    ui.icon(t.chevron_icon).style(t.chevron).shrink(0)
}

/// The panel of a `Popover` menu; `children` scroll inside it when it is
/// given less height than they take.
pub fn menu<'a, M: Copy>(ui: &'a Ui<M>, children: impl Children<'a, M>) -> Element<'a, M> {
    ui.column([ui.scroll(ui.column(children))]).style(ui.theme().widgets.menu.panel).part_of(Part::Panel).id(id!("menu"))
}

/// A choice in a `menu`. `.checked()` shows the theme's check at the end,
/// which takes no room otherwise.
pub fn menu_item<'a, M: Copy>(ui: &'a Ui<M>, leading: Option<Icon>, label: &'a str, on_tap: M) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.menu;
    let lead = leading.map_or(ui.empty(), |icon| ui.icon(icon).style(t.icon).shrink(0).part_of(Part::Leading));
    let check = ui.icon(t.check_icon).style(t.check).inherit_state(PART_STATES).shrink(0).part_of(Part::Trailing);
    ui.row([lead, ui.text(theme.fonts.body, label).style(t.label).ellipsis(Elide::End).grow(1).part_of(Part::Label), check])
        .style(t.item)
        .align(Align::Center)
        .state_source()
        .role(Role::Button)
        .on_tap(on_tap)
}

/// A title over the `menu_item`s after it.
pub fn menu_header<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.caption, text).style(theme.widgets.menu.header).ellipsis(Elide::End).part_of(Part::Title)
}

pub fn menu_divider<'a, M: Copy>(ui: &'a Ui<M>) -> Element<'a, M> {
    divider(ui).style(ui.theme().widgets.menu.divider)
}

/// A dialog for a screen on the `Overlay`, centered in what the insets leave,
/// the keyboard included. `children` stack in it as they are.
pub fn modal<'a, M: Copy>(ui: &'a Ui<M>, children: impl Children<'a, M>) -> Element<'a, M> {
    let t = &ui.theme().widgets.modal;
    let (width, insets) = (ui.screen_size().0, ui.insets());
    let room = width - insets.left - insets.right - t.margin * 2;
    let panel = ui
        .column(children)
        .style(t.panel)
        .width(t.width)
        .max_width(room)
        .appear(Presence::fade().offset(0, 24).ms(150))
        .disappear(Presence::fade().ms(100))
        .part_of(Part::Panel);
    ui.column([panel])
        .padding_each(insets.left + t.margin, insets.top, insets.right + t.margin, insets.bottom)
        .align(Align::Center)
        .justify(Justify::Center)
        .absolute(Some(0), Some(0), Some(0), Some(0))
}

pub fn modal_title<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.title, text).wrap().style(theme.widgets.modal.title).part_of(Part::Title)
}

pub fn modal_message<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    ui.text(theme.fonts.body, text).wrap().style(theme.widgets.modal.message).part_of(Part::Message)
}

/// Buttons side by side at equal widths.
pub fn modal_actions<'a, M: Copy>(ui: &'a Ui<M>, buttons: impl IntoIterator<Item = Element<'a, M>>) -> Element<'a, M> {
    let buttons: Vec<_> = buttons.into_iter().map(|button| button.grow(1)).collect();
    ui.row(buttons).style(ui.theme().widgets.modal.actions).part_of(Part::Actions)
}

pub fn toast<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.toast;
    ui.row([ui.text(theme.fonts.caption, text).style(t.label)]).style(t.root)
}

fn editable<'a, M: Copy>(ui: &'a Ui<M>, text: &'a str, cursor: usize, underline: Option<(usize, usize)>, style: &'static StateStyle, on_input: fn(TextInput) -> M) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.text_field;
    let element = ui.text(theme.fonts.body, text).edit(cursor, underline).style(style);
    let element = if text.is_empty() { element.foreground(t.placeholder_color) } else { element };
    element.role(Role::TextField).on_input(on_input)
}

/// One line of text. `buffer` belongs to the caller, which applies what
/// `on_input` reports.
pub fn text_field<'a, M: Copy>(ui: &'a Ui<M>, buffer: &'a TextBuffer, on_input: fn(TextInput) -> M) -> Element<'a, M> {
    editable(ui, buffer.as_str(), buffer.cursor(), buffer.composing(), ui.theme().widgets.text_field.root, on_input)
}

/// One line of text shown as a mark per character unless `shown`, with a
/// button at its end sending `on_toggle`. A character typed into it shows for
/// the theme's `reveal_ms`.
pub fn password_field<'a, M: Copy>(ui: &'a Ui<M>, buffer: &'a TextBuffer, shown: bool, on_input: fn(TextInput) -> M, on_toggle: M) -> Element<'a, M> {
    let theme = ui.theme();
    let t = &theme.widgets.text_field;
    let field = editable(ui, buffer.as_str(), buffer.cursor(), None, t.password, on_input);
    let field = if shown { field } else { field.secure() };
    let icon = if shown { t.hide_icon } else { t.show_icon };
    let toggle = ui.row([ui.icon(icon)]).style(t.reveal).keep_focus().role(Role::Button).on_tap(on_toggle).absolute(None, Some(0), Some(0), Some(0)).part_of(Part::Trailing);
    ui.column([field, toggle])
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
