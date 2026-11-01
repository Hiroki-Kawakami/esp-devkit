/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::{Color, FontChain};

use crate::keyboard::{Keyboard, NUMERIC, QWERTY};
use crate::style::{Shadow, State};

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

    pub const fn menu_shadow(&self) -> Shadow {
        Shadow { y: 12, blur: 32, color: self.shadow }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ColorMode {
    #[default]
    Light,
    Dark,
}

#[derive(Clone, Copy)]
pub struct Metrics {
    pub bar_height: i16,
    pub row_height: i16,
    pub touch_size: i16,
    pub padding: i16,
    pub gap: i16,
    pub radius: u8,
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
    pub shift: Icon,
    pub backspace: Icon,
    pub enter: Icon,
}

/// The on-screen keyboard of a theme.
#[derive(Clone, Copy)]
pub struct Keyboards {
    pub text: &'static Keyboard,
    pub number: &'static Keyboard,
    /// `None` is the theme's body font.
    pub font: Option<&'static FontChain>,
    pub key_height: i16,
    pub gap: i16,
    pub key: &'static WidgetLook,
    pub special: &'static WidgetLook,
    pub primary: &'static WidgetLook,
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

/// The looks widgets take by default.
#[derive(Clone, Copy)]
pub struct Looks {
    pub button: &'static WidgetLook,
    pub button_secondary: &'static WidgetLook,
    pub icon_button: &'static WidgetLook,
    pub row: &'static WidgetLook,
    pub menu_item: &'static WidgetLook,
    pub text_field: &'static WidgetLook,
    pub key: &'static WidgetLook,
    pub key_special: &'static WidgetLook,
}

/// What a theme's definition gives, generated by resgen.
pub struct ThemeAssets {
    pub light: Palette,
    /// `None` stays light.
    pub dark: Option<Palette>,
    pub metrics: Metrics,
    pub fonts: Fonts,
    pub icons: Icons,
}

/// How widgets are built on any assets.
#[derive(Clone, Copy)]
pub struct WidgetStyle {
    pub keyboards: Keyboards,
    pub motion: Motion,
    pub looks: Looks,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub light: Palette,
    pub dark: Option<Palette>,
    pub metrics: Metrics,
    pub fonts: Fonts,
    pub icons: Icons,
    pub keyboards: Keyboards,
    pub motion: Motion,
    pub looks: Looks,
}

impl Theme {
    pub const fn new(assets: &ThemeAssets, widgets: &WidgetStyle) -> Self {
        Self {
            light: assets.light,
            dark: assets.dark,
            metrics: assets.metrics,
            fonts: assets.fonts,
            icons: assets.icons,
            keyboards: widgets.keyboards,
            motion: widgets.motion,
            looks: widgets.looks,
        }
    }

    pub(crate) fn palette(&self, mode: ColorMode) -> &Palette {
        match (mode, &self.dark) {
            (ColorMode::Dark, Some(dark)) => dark,
            _ => &self.light,
        }
    }
}

const DISABLED: (State, Look) = (State::DISABLED, Look::opacity(102));
const PRESSED: (State, Look) = (State::PRESSED, Look::background(ColorRole::Pressed));
const FRAMED: Look = Look { border: Some(LookBorder { color: ColorRole::Divider, width: 2 }), ..Look::fill(ColorRole::Surface, ColorRole::OnSurface) };

static BUTTON: WidgetLook = WidgetLook {
    base: Look::fill(ColorRole::Primary, ColorRole::OnPrimary),
    states: &[(State::PRESSED, Look::background(ColorRole::PrimaryPressed)), DISABLED],
};
static BUTTON_SECONDARY: WidgetLook = WidgetLook { base: FRAMED, states: &[PRESSED, DISABLED] };
static ICON_BUTTON: WidgetLook = WidgetLook { base: Look::foreground(ColorRole::OnSurface), states: &[PRESSED, DISABLED] };
static ROW: WidgetLook = WidgetLook {
    base: Look::fill(ColorRole::Surface, ColorRole::OnSurface),
    states: &[(State::SELECTED, Look::background(ColorRole::Selected)), PRESSED, DISABLED],
};
static MENU_ITEM: WidgetLook = WidgetLook { base: Look::foreground(ColorRole::OnSurface), states: &[PRESSED, DISABLED] };
static TEXT_FIELD: WidgetLook = WidgetLook { base: FRAMED, states: &[(State::FOCUSED, Look::outline(ColorRole::Primary, 2)), DISABLED] };
static KEY: WidgetLook = WidgetLook { base: Look::fill(ColorRole::Surface, ColorRole::OnSurface), states: &[PRESSED] };
static KEY_SPECIAL: WidgetLook = WidgetLook { base: Look::fill(ColorRole::Divider, ColorRole::OnSurface), states: &[PRESSED] };

/// The widgets as they come.
pub static DEFAULT: WidgetStyle = WidgetStyle {
    keyboards: Keyboards {
        text: &QWERTY,
        number: &NUMERIC,
        font: None,
        key_height: 80,
        gap: 8,
        key: &KEY,
        special: &KEY_SPECIAL,
        primary: &BUTTON,
    },
    motion: Motion { transition: Transition::Slide, duration_ms: 200 },
    looks: Looks {
        button: &BUTTON,
        button_secondary: &BUTTON_SECONDARY,
        icon_button: &ICON_BUTTON,
        row: &ROW,
        menu_item: &MENU_ITEM,
        text_field: &TEXT_FIELD,
        key: &KEY,
        key_special: &KEY_SPECIAL,
    },
};
