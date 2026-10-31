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
    pub divider: Color,
    pub pressed: Color,
    pub selected: Color,
    pub scrim: Color,
    pub shadow: Color,
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
    pub font: &'static FontChain,
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

/// Parts of a widget's appearance; `None` leaves what it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Look {
    pub background: Option<Color>,
    /// Text and icons inside that set no color of their own.
    pub foreground: Option<Color>,
    pub border: Option<Border>,
    pub opacity: Option<u8>,
}

impl Look {
    pub const NONE: Self = Self { background: None, foreground: None, border: None, opacity: None };

    pub const fn fill(background: Color, foreground: Color) -> Self {
        Self { background: Some(background), foreground: Some(foreground), border: None, opacity: None }
    }

    pub const fn background(background: Color) -> Self {
        Self { background: Some(background), ..Self::NONE }
    }

    pub const fn outline(color: Color, width: u8) -> Self {
        Self { border: Some(Border { color, width }), ..Self::NONE }
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

#[derive(Clone, Copy)]
pub struct Theme {
    pub colors: Palette,
    pub metrics: Metrics,
    pub fonts: Fonts,
    pub icons: Icons,
    pub keyboards: Keyboards,
    pub motion: Motion,
    pub looks: Looks,
}

const DISABLED: (State, Look) = (State::DISABLED, Look::opacity(102));

macro_rules! looks {
    ($name:ident, $primary:expr, $on_primary:expr, $primary_pressed:expr, $surface:expr, $on_surface:expr, $pressed:expr, $divider:expr, $selected:expr) => {
        const $name: Looks = {
            static BUTTON: WidgetLook = WidgetLook {
                base: Look::fill($primary, $on_primary),
                states: &[(State::PRESSED, Look::background($primary_pressed)), DISABLED],
            };
            static BUTTON_SECONDARY: WidgetLook = WidgetLook {
                base: Look { border: Some(Border { color: $divider, width: 2 }), ..Look::fill($surface, $on_surface) },
                states: &[(State::PRESSED, Look::background($pressed)), DISABLED],
            };
            static ICON_BUTTON: WidgetLook = WidgetLook {
                base: Look { foreground: Some($on_surface), ..Look::NONE },
                states: &[(State::PRESSED, Look::background($pressed)), DISABLED],
            };
            static ROW: WidgetLook = WidgetLook {
                base: Look::fill($surface, $on_surface),
                states: &[(State::SELECTED, Look::background($selected)), (State::PRESSED, Look::background($pressed)), DISABLED],
            };
            static MENU_ITEM: WidgetLook = WidgetLook {
                base: Look { foreground: Some($on_surface), ..Look::NONE },
                states: &[(State::PRESSED, Look::background($pressed)), DISABLED],
            };
            static TEXT_FIELD: WidgetLook = WidgetLook {
                base: Look { border: Some(Border { color: $divider, width: 2 }), ..Look::fill($surface, $on_surface) },
                states: &[(State::FOCUSED, Look::outline($primary, 2)), DISABLED],
            };
            static KEY: WidgetLook = WidgetLook { base: Look::fill($surface, $on_surface), states: &[(State::PRESSED, Look::background($pressed))] };
            static KEY_SPECIAL: WidgetLook = WidgetLook { base: Look::fill($divider, $on_surface), states: &[(State::PRESSED, Look::background($pressed))] };
            Looks {
                button: &BUTTON,
                button_secondary: &BUTTON_SECONDARY,
                icon_button: &ICON_BUTTON,
                row: &ROW,
                menu_item: &MENU_ITEM,
                text_field: &TEXT_FIELD,
                key: &KEY,
                key_special: &KEY_SPECIAL,
            }
        };
    };
}

looks!(LIGHT_LOOKS, Color::hex(0x2196f3), Color::hex(0xffffff), Color::hex(0x1976d2), Color::hex(0xffffff), Color::hex(0x1d2126), Color::hex(0xdfe3e6), Color::hex(0xd9dde0), Color::hex(0xe3f2fd));
looks!(DARK_LOOKS, Color::hex(0x64b5f6), Color::hex(0x10151a), Color::hex(0x90caf9), Color::hex(0x1b1e22), Color::hex(0xe8eaed), Color::hex(0x2a2f35), Color::hex(0x33383e), Color::hex(0x1e3a52));

const TOUCH_METRICS: Metrics = Metrics { bar_height: 80, row_height: 80, touch_size: 64, padding: 24, gap: 12, radius: 14 };
const SLIDE: Motion = Motion { transition: Transition::Slide, duration_ms: 200 };

impl Theme {
    const fn keyboards(fonts: &Fonts, looks: &Looks) -> Keyboards {
        Keyboards {
            text: &QWERTY,
            number: &NUMERIC,
            font: fonts.body,
            key_height: 80,
            gap: 8,
            key: looks.key,
            special: looks.key_special,
            primary: looks.button,
        }
    }

    pub const fn light(fonts: Fonts, icons: Icons) -> Self {
        Self {
            colors: Palette {
                surface: Color::hex(0xffffff),
                background: Color::hex(0xe9eaec),
                on_surface: Color::hex(0x1d2126),
                muted: Color::hex(0x78909c),
                primary: Color::hex(0x2196f3),
                on_primary: Color::hex(0xffffff),
                divider: Color::hex(0xd9dde0),
                pressed: Color::hex(0xdfe3e6),
                selected: Color::hex(0xe3f2fd),
                scrim: Color::rgba(0, 0, 0, 128),
                shadow: Color::rgba(17, 20, 24, 56),
            },
            metrics: TOUCH_METRICS,
            fonts,
            icons,
            keyboards: Self::keyboards(&fonts, &LIGHT_LOOKS),
            motion: SLIDE,
            looks: LIGHT_LOOKS,
        }
    }

    pub const fn dark(fonts: Fonts, icons: Icons) -> Self {
        Self {
            colors: Palette {
                surface: Color::hex(0x1b1e22),
                background: Color::hex(0x101214),
                on_surface: Color::hex(0xe8eaed),
                muted: Color::hex(0x90a4ae),
                primary: Color::hex(0x64b5f6),
                on_primary: Color::hex(0x10151a),
                divider: Color::hex(0x33383e),
                pressed: Color::hex(0x2a2f35),
                selected: Color::hex(0x1e3a52),
                scrim: Color::rgba(0, 0, 0, 160),
                shadow: Color::rgba(0, 0, 0, 96),
            },
            metrics: TOUCH_METRICS,
            fonts,
            icons,
            keyboards: Self::keyboards(&fonts, &DARK_LOOKS),
            motion: SLIDE,
            looks: DARK_LOOKS,
        }
    }

    pub const fn menu_shadow(&self) -> Shadow {
        Shadow { y: 12, blur: 32, color: self.colors.shadow }
    }
}
