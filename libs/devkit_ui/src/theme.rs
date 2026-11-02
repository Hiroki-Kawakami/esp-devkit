/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::{Color, FontChain};

use crate::keyboard::Keyboard;
use crate::style::StateStyle;

#[derive(Clone, Copy)]
pub struct Palette {
    pub text: Color,
    pub primary: Color,
    pub surface: Color,
    pub background: Color,
    pub divider: Color,
    pub custom: &'static [Color],
}

/// A color by what it is for, so styles work with any palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorRole {
    Text,
    Primary,
    Surface,
    Background,
    Divider,
    Custom(u8),
    Fixed(Color),
}

impl From<Color> for ColorRole {
    fn from(color: Color) -> Self {
        Self::Fixed(color)
    }
}

impl Palette {
    pub const fn color(&self, role: ColorRole) -> Color {
        match role {
            ColorRole::Text => self.text,
            ColorRole::Primary => self.primary,
            ColorRole::Surface => self.surface,
            ColorRole::Background => self.background,
            ColorRole::Divider => self.divider,
            ColorRole::Custom(index) => self.custom[index as usize],
            ColorRole::Fixed(color) => color,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ColorMode {
    #[default]
    Light,
    Dark,
}

#[derive(Clone, Copy)]
pub struct Fonts {
    pub title: &'static FontChain,
    pub body: &'static FontChain,
    pub caption: &'static FontChain,
}

/// A glyph of an icon font.
#[derive(Clone, Copy)]
pub struct Icon {
    pub font: &'static FontChain,
    pub glyph: char,
}

/// The icons widgets use. Apps give them from their own resources.
#[derive(Clone, Copy)]
pub struct Icons {
    pub back: Icon,
    pub more: Icon,
    pub close: Icon,
    pub check: Icon,
    pub chevron: Icon,
    pub expand: Icon,
    pub shift: Icon,
    pub backspace: Icon,
    pub enter: Icon,
}

/// The on-screen keyboards of a theme.
#[derive(Clone, Copy)]
pub struct Keyboards {
    pub text: &'static Keyboard,
    pub number: &'static Keyboard,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transition {
    /// Screens replace each other at once, as a slow panel needs.
    None,
    Slide,
}

#[derive(Clone, Copy)]
pub struct Motion {
    pub transition: Transition,
    pub duration_ms: u16,
}

/// Base values, for widgets and apps alike; no widget of its own.
#[derive(Clone, Copy)]
pub struct UnitTheme {
    pub padding: i16,
    pub gap: i16,
    pub radius: u8,
}

#[derive(Clone, Copy)]
pub struct ButtonTheme {
    pub root: &'static StateStyle,
    pub label: &'static StateStyle,
    pub primary: &'static StateStyle,
    pub secondary: &'static StateStyle,
    pub destructive: &'static StateStyle,
    pub plain: &'static StateStyle,
    pub navigation: &'static StateStyle,
    pub primary_outline: &'static StateStyle,
    pub secondary_outline: &'static StateStyle,
    pub destructive_outline: &'static StateStyle,
    pub plain_outline: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct IconButtonTheme {
    pub root: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct ListTheme {
    pub row_height: i16,
}

#[derive(Clone, Copy)]
pub struct ListItemTheme {
    pub root: &'static StateStyle,
    pub text: &'static StateStyle,
    pub title: &'static StateStyle,
    pub meta: &'static StateStyle,
    pub leading: &'static StateStyle,
    pub trailing: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct TopBarTheme {
    pub back_height: i16,
    pub padding_x: i16,
    pub root: &'static StateStyle,
    pub title: &'static StateStyle,
    pub back: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct SwitchTheme {
    pub root: &'static StateStyle,
    pub thumb: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct CheckboxTheme {
    pub root: &'static StateStyle,
    pub mark: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct SliderTheme {
    pub inset: i16,
    pub track_inset: i16,
    pub root: &'static StateStyle,
    pub track: &'static StateStyle,
    pub fill: &'static StateStyle,
    pub thumb: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct SegmentedTheme {
    pub root: &'static StateStyle,
    pub item: &'static StateStyle,
    pub label: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct DropdownTheme {
    pub height: i16,
    pub root: &'static StateStyle,
    pub label: &'static StateStyle,
    pub icon: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct DropdownMenuTheme {
    pub root: &'static StateStyle,
    pub item: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct TableTheme {
    pub padding: i16,
}

#[derive(Clone, Copy)]
pub struct TableHeaderTheme {
    pub root: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct TableSectionTheme {
    pub root: &'static StateStyle,
    pub divider: &'static StateStyle,
    pub line: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct TableRowTheme {
    pub root: &'static StateStyle,
    pub label: &'static StateStyle,
    pub value: &'static StateStyle,
    pub chevron: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct MenuTheme {
    pub width: i16,
    pub padding_y: i16,
    pub panel: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct MenuItemTheme {
    pub height: i16,
    pub root: &'static StateStyle,
    pub label: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct ModalTheme {
    pub width: i16,
    pub scrim: &'static StateStyle,
    pub panel: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct ToastTheme {
    pub root: &'static StateStyle,
    pub label: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct TextFieldTheme {
    pub placeholder_color: ColorRole,
    pub caret_color: ColorRole,
    pub underline_color: ColorRole,
    pub area_min_height: i16,
    pub root: &'static StateStyle,
}

#[derive(Clone, Copy)]
pub struct KeyboardTheme {
    pub gap: i16,
    pub root: &'static StateStyle,
    pub label: &'static StateStyle,
    pub key: &'static StateStyle,
    pub special: &'static StateStyle,
    pub primary: &'static StateStyle,
}

/// Each widget's values, generated by resgen.
#[derive(Clone, Copy)]
pub struct Widgets {
    pub unit: UnitTheme,
    pub button: ButtonTheme,
    pub icon_button: IconButtonTheme,
    pub list: ListTheme,
    pub list_item: ListItemTheme,
    pub top_bar: TopBarTheme,
    pub switch: SwitchTheme,
    pub checkbox: CheckboxTheme,
    pub slider: SliderTheme,
    pub segmented: SegmentedTheme,
    pub dropdown: DropdownTheme,
    pub dropdown_menu: DropdownMenuTheme,
    pub table: TableTheme,
    pub table_header: TableHeaderTheme,
    pub table_section: TableSectionTheme,
    pub table_row: TableRowTheme,
    pub menu: MenuTheme,
    pub menu_item: MenuItemTheme,
    pub modal: ModalTheme,
    pub toast: ToastTheme,
    pub text_field: TextFieldTheme,
    pub keyboard: KeyboardTheme,
}

/// What a theme's definition gives, generated by resgen.
#[derive(Clone, Copy)]
pub struct Theme {
    pub light: Palette,
    /// `None` stays light.
    pub dark: Option<Palette>,
    pub fonts: Fonts,
    pub icons: Icons,
    pub widgets: Widgets,
    pub keyboards: Keyboards,
    pub motion: Motion,
}

impl Theme {
    pub(crate) fn palette(&self, mode: ColorMode) -> &Palette {
        match (mode, &self.dark) {
            (ColorMode::Dark, Some(dark)) => dark,
            _ => &self.light,
        }
    }
}
