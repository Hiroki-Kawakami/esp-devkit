/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::Color;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Direction {
    #[default]
    Column,
    Row,
    /// Equal columns, rows as tall as their tallest cell.
    Grid(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Dimension {
    #[default]
    Auto,
    Px(i16),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Align {
    #[default]
    Stretch,
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Role {
    #[default]
    None,
    Button,
    Text,
    List,
    Header,
}

impl Role {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::None => "box",
            Self::Button => "button",
            Self::Text => "text",
            Self::List => "list",
            Self::Header => "header",
        }
    }
}

/// Reaches `blur` px around the box, moved down by `y`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shadow {
    pub y: i8,
    pub blur: u8,
    pub color: Color,
}

/// Distances from the parent's edges, for a child outside the flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Inset {
    pub left: Option<i16>,
    pub top: Option<i16>,
    pub right: Option<i16>,
    pub bottom: Option<i16>,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Style {
    pub(crate) direction: Direction,
    pub(crate) width: Dimension,
    pub(crate) height: Dimension,
    pub(crate) min_width: i16,
    pub(crate) min_height: i16,
    pub(crate) grow: u8,
    pub(crate) aspect: Option<(u8, u8)>,
    pub(crate) padding: [i16; 4],
    pub(crate) gap_x: i16,
    pub(crate) gap_y: i16,
    pub(crate) align: Align,
    pub(crate) align_self: Option<Align>,
    pub(crate) justify: Justify,
    pub(crate) absolute: Option<Inset>,

    pub(crate) background: Option<Color>,
    pub(crate) pressed_background: Option<Color>,
    pub(crate) radius: u8,
    pub(crate) border_top: Option<Color>,
    pub(crate) border_bottom: Option<Color>,
    pub(crate) shadow: Option<Shadow>,
    pub(crate) opacity: Option<u8>,
    pub(crate) clip: bool,
}

impl Style {
    pub(crate) fn layout_eq(&self, other: &Style) -> bool {
        self.direction == other.direction
            && self.width == other.width
            && self.height == other.height
            && self.min_width == other.min_width
            && self.min_height == other.min_height
            && self.grow == other.grow
            && self.aspect == other.aspect
            && self.padding == other.padding
            && self.gap_x == other.gap_x
            && self.gap_y == other.gap_y
            && self.align == other.align
            && self.align_self == other.align_self
            && self.justify == other.justify
            && self.absolute == other.absolute
    }
}

/// What a screen wants from the chrome around it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ScreenStyle {
    pub bars: Bars,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Bars {
    /// Dark content on a light screen.
    #[default]
    Dark,
    /// Light content on a dark screen.
    Light,
    Hidden,
}

/// Space the chrome takes from each edge of the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Insets {
    pub top: i16,
    pub bottom: i16,
    pub left: i16,
    pub right: i16,
}
