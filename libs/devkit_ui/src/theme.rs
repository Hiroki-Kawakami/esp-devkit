/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::{Color, FontChain};

use crate::keyboard::{Keyboard, NUMERIC, QWERTY};
use crate::style::State;

#[derive(Clone, Copy)]
pub struct Palette {
    pub surface: Color,
    pub background: Color,
    pub on_surface: Color,
    pub muted: Color,
    pub primary: Color,
    pub on_primary: Color,
    pub primary_pressed: Color,
    pub divider: Color,
    pub pressed: Color,
    pub selected: Color,
    pub scrim: Color,
    pub shadow: Color,
}

/// A color by what it is for, so looks work with any palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorRole {
    Surface,
    Background,
    OnSurface,
    Muted,
    Primary,
    OnPrimary,
    PrimaryPressed,
    Divider,
    Pressed,
    Selected,
    Scrim,
    Shadow,
    Fixed(Color),
}

impl Palette {
    pub const fn color(&self, role: ColorRole) -> Color {
        match role {
            ColorRole::Surface => self.surface,
            ColorRole::Background => self.background,
            ColorRole::OnSurface => self.on_surface,
            ColorRole::Muted => self.muted,
            ColorRole::Primary => self.primary,
            ColorRole::OnPrimary => self.on_primary,
            ColorRole::PrimaryPressed => self.primary_pressed,
            ColorRole::Divider => self.divider,
            ColorRole::Pressed => self.pressed,
            ColorRole::Selected => self.selected,
            ColorRole::Scrim => self.scrim,
            ColorRole::Shadow => self.shadow,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Border {
    pub color: Color,
    pub width: u8,
}

/// A `Border` by role, for a `Look`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LookBorder {
    pub color: ColorRole,
    pub width: u8,
}

/// Parts of a widget's appearance; `None` leaves what it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Look {
    pub background: Option<ColorRole>,
    /// Text and icons inside that set no color of their own.
    pub foreground: Option<ColorRole>,
    pub border: Option<LookBorder>,
    pub opacity: Option<u8>,
}

/// A `Look` in the colors of a palette.
#[derive(Clone, Copy)]
pub(crate) struct Colored {
    pub(crate) background: Option<Color>,
    pub(crate) foreground: Option<Color>,
    pub(crate) border: Option<Border>,
    pub(crate) opacity: Option<u8>,
}

impl Look {
    pub const NONE: Self = Self { background: None, foreground: None, border: None, opacity: None };

    pub const fn fill(background: ColorRole, foreground: ColorRole) -> Self {
        Self { background: Some(background), foreground: Some(foreground), border: None, opacity: None }
    }

    pub const fn background(background: ColorRole) -> Self {
        Self { background: Some(background), ..Self::NONE }
    }

    pub const fn foreground(foreground: ColorRole) -> Self {
        Self { foreground: Some(foreground), ..Self::NONE }
    }

    pub const fn outline(color: ColorRole, width: u8) -> Self {
        Self { border: Some(LookBorder { color, width }), ..Self::NONE }
    }

    pub const fn opacity(opacity: u8) -> Self {
        Self { opacity: Some(opacity), ..Self::NONE }
    }

    pub(crate) fn over(self, base: Self) -> Self {
        Self {
            background: self.background.or(base.background),
            foreground: self.foreground.or(base.foreground),
            border: self.border.or(base.border),
            opacity: self.opacity.or(base.opacity),
        }
    }

    pub(crate) fn colored(self, palette: &Palette) -> Colored {
        Colored {
            background: self.background.map(|role| palette.color(role)),
            foreground: self.foreground.map(|role| palette.color(role)),
            border: self.border.map(|b| Border { color: palette.color(b.color), width: b.width }),
            opacity: self.opacity,
        }
    }
}

/// A widget's appearance: the base, then each entry whose states are all set,
/// in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WidgetLook {
    pub base: Look,
    pub states: &'static [(State, Look)],
}

impl WidgetLook {
    pub(crate) fn resolve(&self, state: State) -> Look {
        self.states.iter().filter(|(s, _)| state.contains(*s)).fold(self.base, |look, (_, over)| over.over(look))
    }
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
    pub height: i16,
    pub padding_x: i16,
    pub radius: u8,
    pub font: &'static FontChain,
    pub primary: &'static WidgetLook,
    pub secondary: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct IconButtonTheme {
    pub size: i16,
    pub radius: u8,
    pub look: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct ListItemTheme {
    pub height: i16,
    pub padding_x: i16,
    pub gap: i16,
    pub text_gap: i16,
    pub title_font: &'static FontChain,
    pub meta_font: &'static FontChain,
    pub leading_color: ColorRole,
    pub meta_color: ColorRole,
    pub trailing_color: ColorRole,
    pub divider_color: ColorRole,
    pub look: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct TopBarTheme {
    pub height: i16,
    pub padding_x: i16,
    pub gap: i16,
    pub font: &'static FontChain,
    pub back_height: i16,
    pub back_radius: u8,
    pub back_look: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct SwitchTheme {
    pub width: i16,
    pub height: i16,
    pub thumb: i16,
    pub on_color: ColorRole,
    pub off_color: ColorRole,
    pub thumb_color: ColorRole,
}

#[derive(Clone, Copy)]
pub struct CheckboxTheme {
    pub size: i16,
    pub radius: u8,
    pub mark_inset: i16,
    pub mark_radius: u8,
    pub on_color: ColorRole,
    pub off_color: ColorRole,
    pub mark_color: ColorRole,
}

#[derive(Clone, Copy)]
pub struct SliderTheme {
    pub height: i16,
    pub track: i16,
    pub thumb: i16,
    pub active_color: ColorRole,
    pub track_color: ColorRole,
    pub thumb_color: ColorRole,
}

#[derive(Clone, Copy)]
pub struct SegmentedTheme {
    pub height: i16,
    pub inset: i16,
    pub radius: u8,
    pub background: ColorRole,
    pub item_padding_x: i16,
    pub item_radius: u8,
    pub font: &'static FontChain,
    pub look: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct DropdownTheme {
    pub width: i16,
    pub height: i16,
    pub padding_x: i16,
    pub gap: i16,
    pub radius: u8,
    pub font: &'static FontChain,
    pub icon_color: ColorRole,
    pub look: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct DropdownMenuTheme {
    pub radius: u8,
    pub look: &'static WidgetLook,
    pub item_look: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct TableTheme {
    pub padding: i16,
}

#[derive(Clone, Copy)]
pub struct TableHeaderTheme {
    pub indent: i16,
    pub top: i16,
    pub bottom: i16,
    pub font: &'static FontChain,
    pub color: ColorRole,
}

#[derive(Clone, Copy)]
pub struct TableSectionTheme {
    pub radius: u8,
    pub background: ColorRole,
    pub divider_color: ColorRole,
    pub divider_inset: i16,
}

#[derive(Clone, Copy)]
pub struct TableRowTheme {
    pub min_height: i16,
    pub padding_x: i16,
    pub gap: i16,
    pub label_font: &'static FontChain,
    pub value_font: &'static FontChain,
    pub value_color: ColorRole,
    pub chevron_color: ColorRole,
    pub look: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct MenuTheme {
    pub width: i16,
    pub padding_y: i16,
    pub radius: u8,
    pub right: i16,
    pub background: ColorRole,
    pub shadow_y: i8,
    pub shadow_blur: u8,
    pub shadow_color: ColorRole,
}

#[derive(Clone, Copy)]
pub struct MenuItemTheme {
    pub height: i16,
    pub padding_x: i16,
    pub font: &'static FontChain,
    pub look: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct ModalTheme {
    pub width: i16,
    pub padding: i16,
    pub radius: u8,
    pub background: ColorRole,
    pub scrim: ColorRole,
}

#[derive(Clone, Copy)]
pub struct ToastTheme {
    pub padding_x: i16,
    pub padding_y: i16,
    pub radius: u8,
    pub margin: i16,
    pub font: &'static FontChain,
    pub background: ColorRole,
    pub foreground: ColorRole,
}

#[derive(Clone, Copy)]
pub struct TextFieldTheme {
    pub padding_x: i16,
    pub padding_y: i16,
    pub radius: u8,
    pub font: &'static FontChain,
    pub placeholder_color: ColorRole,
    pub area_min_height: i16,
    pub look: &'static WidgetLook,
}

#[derive(Clone, Copy)]
pub struct KeyboardTheme {
    pub key_height: i16,
    pub gap: i16,
    pub radius: u8,
    pub font: &'static FontChain,
    pub background: ColorRole,
    pub key: &'static WidgetLook,
    pub special: &'static WidgetLook,
    pub primary: &'static WidgetLook,
}

/// Each widget's values, generated by resgen.
#[derive(Clone, Copy)]
pub struct Widgets {
    pub unit: UnitTheme,
    pub button: ButtonTheme,
    pub icon_button: IconButtonTheme,
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
pub struct ThemeAssets {
    pub light: Palette,
    /// `None` stays light.
    pub dark: Option<Palette>,
    pub fonts: Fonts,
    pub icons: Icons,
    pub widgets: Widgets,
}

/// How widgets are built on any assets.
#[derive(Clone, Copy)]
pub struct WidgetStyle {
    pub keyboards: Keyboards,
    pub motion: Motion,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub light: Palette,
    pub dark: Option<Palette>,
    pub fonts: Fonts,
    pub icons: Icons,
    pub widgets: Widgets,
    pub keyboards: Keyboards,
    pub motion: Motion,
}

impl Theme {
    pub const fn new(assets: &ThemeAssets, widgets: &WidgetStyle) -> Self {
        Self {
            light: assets.light,
            dark: assets.dark,
            fonts: assets.fonts,
            icons: assets.icons,
            widgets: assets.widgets,
            keyboards: widgets.keyboards,
            motion: widgets.motion,
        }
    }

    pub(crate) fn palette(&self, mode: ColorMode) -> &Palette {
        match (mode, &self.dark) {
            (ColorMode::Dark, Some(dark)) => dark,
            _ => &self.light,
        }
    }
}

/// The widgets as they come.
pub static DEFAULT: WidgetStyle = WidgetStyle {
    keyboards: Keyboards { text: &QWERTY, number: &NUMERIC },
    motion: Motion { transition: Transition::Slide, duration_ms: 200 },
};
