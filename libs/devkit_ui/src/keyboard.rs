/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use devkit_gfx::FontChain;

use crate::component::{Child, Component, Cx};
use crate::element::{Element, Ui};
use crate::style::{Align, Justify, Role, StateStyle};
use crate::text_buffer::TextInput;
use crate::theme::{Icon, Icons};

pub type LayerId = u8;

const QUARTER: u8 = 4;
const DOUBLE_TAP_MS: u32 = 350;

/// When the on-screen keyboard shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyboardMode {
    /// While a text input has focus and no physical keyboard is connected.
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy)]
pub enum KeyLabel {
    None,
    Text(&'static str),
    /// An icon of the theme.
    Icon(fn(&Icons) -> Icon),
    Fixed(Icon),
}

#[derive(Clone, Copy)]
pub struct LayerChange {
    to: LayerId,
    once: bool,
    double: Option<LayerId>,
}

#[derive(Clone, Copy)]
pub enum KeyAction {
    None,
    Insert(&'static str),
    Press(TextInput),
    Layer(LayerChange),
    /// Reaches the focused input as `TextInput::Custom`.
    Custom(u16),
}

impl KeyAction {
    pub const fn layer(to: LayerId) -> Self {
        Self::Layer(LayerChange { to, once: false, double: None })
    }

    /// Of a layer change: back to the previous layer after one character.
    pub const fn once(mut self) -> Self {
        if let Self::Layer(change) = &mut self {
            change.once = true;
        }
        self
    }

    /// Of a layer change: where a double tap goes.
    pub const fn double(mut self, to: LayerId) -> Self {
        if let Self::Layer(change) = &mut self {
            change.double = Some(to);
        }
        self
    }
}

/// Which of the theme's key looks a key takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyRole {
    Normal,
    Special,
    Primary,
}

/// One key. A key `n` columns wide covers `n` pitches less one gap, so keys line
/// up with the rows above and below.
#[derive(Clone, Copy)]
pub struct Key {
    label: KeyLabel,
    action: KeyAction,
    width: u8,
    role: KeyRole,
    style: Option<&'static StateStyle>,
    font: Option<&'static FontChain>,
    repeat: bool,
}

impl Key {
    pub const fn action(label: KeyLabel, action: KeyAction) -> Self {
        Self { label, action, width: QUARTER, role: KeyRole::Normal, style: None, font: None, repeat: false }
    }

    /// A key typing its own label.
    pub const fn text(text: &'static str) -> Self {
        Self::action(KeyLabel::Text(text), KeyAction::Insert(text))
    }

    pub const fn space() -> Self {
        Self::action(KeyLabel::None, KeyAction::Insert(" "))
    }

    /// Empty space, for indented rows.
    pub const fn spacer(columns: f32) -> Self {
        Self::action(KeyLabel::None, KeyAction::None).width(columns)
    }

    /// In columns, by quarters.
    pub const fn width(mut self, columns: f32) -> Self {
        let quarters = columns * QUARTER as f32;
        assert!(quarters as u8 as f32 == quarters, "key widths go by quarters");
        self.width = quarters as u8;
        self
    }

    pub const fn role(mut self, role: KeyRole) -> Self {
        self.role = role;
        self
    }

    pub const fn style(mut self, style: &'static StateStyle) -> Self {
        self.style = Some(style);
        self
    }

    pub const fn font(mut self, font: &'static FontChain) -> Self {
        self.font = Some(font);
        self
    }

    /// Repeats while held.
    pub const fn repeat(mut self) -> Self {
        self.repeat = true;
        self
    }
}

pub struct Layer {
    /// Key widths of a full row.
    pub columns: u8,
    pub rows: &'static [&'static [Key]],
}

pub struct Keyboard {
    /// Of the whole panel, whichever layer shows. Rows share it evenly.
    pub height: i16,
    pub layers: &'static [Layer],
}

/// The kind of text an input takes, choosing the theme's keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputKind {
    Text,
    Number,
}

#[derive(Clone, Copy)]
pub enum KeyboardMsg {
    Key(u8, u8),
    Panel,
}

/// What `widgets::keyboard` embeds.
#[derive(Clone, Copy)]
pub struct KeyboardProps {
    pub keyboard: &'static Keyboard,
    pub width: i16,
}

impl PartialEq for KeyboardProps {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.keyboard, other.keyboard) && self.width == other.width
    }
}

/// An on-screen keyboard, typing into the focused input.
pub struct KeyboardPanel {
    layout: Option<&'static Keyboard>,
    width: Option<i16>,
    layer: LayerId,
    after_once: Option<LayerId>,
    last_tap: Option<(u8, u8, u32)>,
}

impl KeyboardPanel {
    pub(crate) fn docked() -> Self {
        Self { layout: None, width: None, layer: 0, after_once: None, last_tap: None }
    }

    pub(crate) fn show(&mut self, layout: Option<&'static Keyboard>) {
        if !matches!((self.layout, layout), (Some(a), Some(b)) if core::ptr::eq(a, b)) {
            self.layer = 0;
            self.after_once = None;
        }
        self.layout = layout;
    }

    fn change_layer(&mut self, change: LayerChange, row: u8, column: u8) {
        let now = idf::time::Instant::now().since_boot().as_millis() as u32;
        let again = self.last_tap.is_some_and(|(r, c, at)| r == row && c == column && now.wrapping_sub(at) < DOUBLE_TAP_MS);
        self.last_tap = Some((row, column, now));
        if let (true, Some(to)) = (again, change.double) {
            self.layer = to;
            self.after_once = None;
            return;
        }
        self.after_once = change.once.then_some(self.layer);
        self.layer = change.to;
    }
}

impl Component for KeyboardPanel {
    type Msg = KeyboardMsg;
    type Props = KeyboardProps;

    fn update(&mut self, msg: KeyboardMsg, cx: &mut Cx<Self>) {
        let KeyboardMsg::Key(row, column) = msg else { return };
        let Some(layout) = self.layout else { return };
        let Some(key) = layout.layers.get(usize::from(self.layer)).and_then(|l| l.rows.get(usize::from(row))).and_then(|r| r.get(usize::from(column))) else { return };
        if !matches!(key.action, KeyAction::Layer(_)) {
            self.last_tap = None;
        }
        match key.action {
            KeyAction::None => {}
            KeyAction::Insert(text) => {
                for c in text.chars() {
                    cx.input(TextInput::Char(c));
                }
                if let Some(back) = self.after_once.take() {
                    self.layer = back;
                    cx.rebuild();
                }
            }
            KeyAction::Press(input) => cx.input(input),
            KeyAction::Custom(id) => cx.input(TextInput::Custom(id)),
            KeyAction::Layer(change) => {
                self.change_layer(change, row, column);
                cx.rebuild();
            }
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<KeyboardMsg>) -> Element<'a, KeyboardMsg> {
        let Some(layout) = self.layout else { return ui.empty() };
        let Some(layer) = layout.layers.get(usize::from(self.layer)) else { return ui.empty() };
        let theme = ui.theme();
        let k = &theme.widgets.keyboard;
        let width = self.width.unwrap_or(ui.screen_size().0);
        let span = i32::from(width) - i32::from(k.gap);
        let columns = i32::from(layer.columns) * QUARTER as i32;
        let span_y = i32::from(layout.height) - i32::from(k.gap);
        let row_count = layer.rows.len() as i32;
        let rows: Vec<_> = layer
            .rows
            .iter()
            .enumerate()
            .map(|(r, keys)| {
                let top = r as i32;
                let height = ((top + 1) * span_y / row_count - top * span_y / row_count) as i16 - k.gap;
                let mut start = 0;
                let items: Vec<_> = keys
                    .iter()
                    .enumerate()
                    .map(|(c, key)| {
                        let end = start + i32::from(key.width);
                        let width = (end * span / columns - start * span / columns) as i16 - k.gap;
                        start = end;
                        key_element(ui, key, r as u8, c as u8, width, height)
                    })
                    .collect();
                ui.row(items).gap(k.gap).height(height)
            })
            .collect();
        let panel = ui.column(rows).height(layout.height).gap(k.gap).padding(k.gap).style(k.root).keep_focus().on_tap(KeyboardMsg::Panel).id("keyboard");
        match self.width {
            Some(width) => panel.width(width),
            None => panel.absolute(Some(0), None, Some(0), Some(0)),
        }
    }
}

impl Child for KeyboardPanel {
    fn create(props: &KeyboardProps) -> Self {
        Self { layout: Some(props.keyboard), width: Some(props.width), layer: 0, after_once: None, last_tap: None }
    }

    fn set_props(&mut self, props: &KeyboardProps) {
        self.show(Some(props.keyboard));
        self.width = Some(props.width);
    }
}

fn key_element<'a>(ui: &'a Ui<KeyboardMsg>, key: &Key, row: u8, column: u8, width: i16, height: i16) -> Element<'a, KeyboardMsg> {
    let theme = ui.theme();
    let k = &theme.widgets.keyboard;
    if matches!(key.action, KeyAction::None) {
        return ui.column([]).width(width);
    }
    let label = match key.label {
        KeyLabel::None => ui.empty(),
        KeyLabel::Text(text) => {
            let label = ui.text(theme.fonts.body, text).style(k.label).ellipsis();
            key.font.map_or(label, |font| label.font(font))
        }
        KeyLabel::Icon(pick) => ui.icon(pick(&theme.icons)),
        KeyLabel::Fixed(icon) => ui.icon(icon),
    };
    let style = key.style.unwrap_or(match key.role {
        KeyRole::Normal => k.key,
        KeyRole::Special => k.special,
        KeyRole::Primary => k.primary,
    });
    ui.row([label]).style(style).width(width).height(height).align(Align::Center).justify(Justify::Center).role(Role::Button).repeat(key.repeat).keep_focus().on_tap(KeyboardMsg::Key(row, column))
}
