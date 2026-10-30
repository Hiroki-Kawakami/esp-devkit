/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::{Color, FontChain};

use crate::style::Shadow;

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
    pub icons: &'static FontChain,
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

/// How widgets draw their surfaces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WidgetStyle {
    Filled,
    Outlined,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub colors: Palette,
    pub metrics: Metrics,
    pub fonts: Fonts,
    pub motion: Motion,
    pub widgets: WidgetStyle,
}

const TOUCH_METRICS: Metrics = Metrics { bar_height: 80, row_height: 80, touch_size: 64, padding: 24, gap: 12, radius: 14 };
const SLIDE: Motion = Motion { transition: Transition::Slide, duration_ms: 200 };

impl Theme {
    pub const fn light(fonts: Fonts) -> Self {
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
            motion: SLIDE,
            widgets: WidgetStyle::Filled,
        }
    }

    pub const fn dark(fonts: Fonts) -> Self {
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
            motion: SLIDE,
            widgets: WidgetStyle::Filled,
        }
    }

    pub const fn menu_shadow(&self) -> Shadow {
        Shadow { y: 12, blur: 32, color: self.colors.shadow }
    }
}
