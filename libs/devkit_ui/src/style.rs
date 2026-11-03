/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::FontChain;

use crate::rotation::Rotations;
use crate::theme::ColorRole;

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
    /// `.0 / .1` of what the parent offers; `Auto` where it offers no limit.
    Fraction(u16, u16),
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

/// A piece of a widget, for `Element::part`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Actions,
    Back,
    Fill,
    Item,
    Label,
    Leading,
    Mark,
    Message,
    Meta,
    Panel,
    Thumb,
    Title,
    Track,
    Trailing,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Role {
    #[default]
    None,
    Button,
    Text,
    List,
    Header,
    TextField,
}

impl Role {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::None => "box",
            Self::Button => "button",
            Self::Text => "text",
            Self::List => "list",
            Self::Header => "header",
            Self::TextField => "text-field",
        }
    }
}

devkit::bitflags! {
    /// What a node is going through, in rising weight: where styles of two
    /// states set the same value, the greater bits win. The framework sets
    /// `PRESSED`, `HOVERED` and `FOCUSED` without a rebuild; views pass the others.
    #[derive(Default)]
    pub struct State: u8 {
        const CHECKED = 1 << 0;
        const SELECTED = 1 << 1;
        const FOCUSED = 1 << 2;
        const HOVERED = 1 << 3;
        const PRESSED = 1 << 4;
        const DISABLED = 1 << 5;
    }
}

devkit::bitflags! {
    /// What a style eases to when it changes. `COLOR` is the background, the
    /// foreground and the border; `POSITION` is where layout puts the node in
    /// its parent.
    #[derive(Default)]
    pub struct Animated: u8 {
        const COLOR = 1 << 0;
        const OPACITY = 1 << 1;
        const POSITION = 1 << 2;
    }
}

/// Reaches `blur` px around the box, moved down by `y`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shadow {
    pub y: i8,
    pub blur: u8,
    pub color: ColorRole,
}

/// Distances from the parent's edges, for a child outside the flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Inset {
    pub left: Option<i16>,
    pub top: Option<i16>,
    pub right: Option<i16>,
    pub bottom: Option<i16>,
}

/// One value of a `Style`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Prop {
    Direction(Direction),
    Width(i16),
    Height(i16),
    WidthFraction(u16, u16),
    HeightFraction(u16, u16),
    MinWidth(i16),
    MinHeight(i16),
    MaxWidth(i16),
    MaxHeight(i16),
    Grow(u8),
    Shrink(u8),
    Aspect(u8, u8),
    Padding(i16),
    PaddingX(i16),
    PaddingY(i16),
    PaddingEach(i16, i16, i16, i16),
    Gap(i16),
    GapX(i16),
    GapY(i16),
    Align(Align),
    AlignSelf(Align),
    Justify(Justify),
    Left(i16),
    Top(i16),
    Right(i16),
    Bottom(i16),
    Background(ColorRole),
    Foreground(ColorRole),
    Radius(u8),
    Border(ColorRole, u8),
    BorderColor(ColorRole),
    BorderWidth(u8),
    BorderTop(ColorRole),
    BorderBottom(ColorRole),
    ScrollBorder(&'static str),
    Shadow(i8, u8, ColorRole),
    Opacity(u8),
    Clip(bool),
    /// The style changing to this one eases `Animated` over the milliseconds.
    Animate(Animated, u16),
}

/// A style to share: values for every node using it, and what each state
/// changes. `font` is for text.
#[derive(Clone, Copy)]
pub struct StateStyle {
    pub font: Option<&'static FontChain>,
    pub base: &'static [Prop],
    pub states: &'static [(State, &'static [Prop])],
}

impl StateStyle {
    pub const EMPTY: Self = Self { font: None, base: &[], states: &[] };
}

impl PartialEq for StateStyle {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self, other)
    }
}

impl core::fmt::Debug for StateStyle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("StateStyle")
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Style {
    pub(crate) direction: Direction,
    pub(crate) width: Dimension,
    pub(crate) height: Dimension,
    pub(crate) min_width: i16,
    pub(crate) min_height: i16,
    /// 0 is no limit.
    pub(crate) max_width: i16,
    pub(crate) max_height: i16,
    pub(crate) grow: u8,
    pub(crate) shrink: u8,
    pub(crate) aspect: Option<(u8, u8)>,
    pub(crate) padding: [i16; 4],
    pub(crate) gap_x: i16,
    pub(crate) gap_y: i16,
    pub(crate) align: Align,
    pub(crate) align_self: Option<Align>,
    pub(crate) justify: Justify,
    pub(crate) absolute: Option<Inset>,

    pub(crate) states: Option<&'static StateStyle>,
    pub(crate) background: Option<ColorRole>,
    /// Text and icons here and inside that set none of their own.
    pub(crate) foreground: Option<ColorRole>,
    pub(crate) radius: u8,
    pub(crate) border_color: Option<ColorRole>,
    pub(crate) border_width: u8,
    pub(crate) border_top: Option<ColorRole>,
    pub(crate) border_bottom: Option<ColorRole>,
    pub(crate) scroll_border: Option<&'static str>,
    pub(crate) shadow: Option<Shadow>,
    pub(crate) opacity: Option<u8>,
    pub(crate) clip: bool,
    pub(crate) animated: Animated,
    pub(crate) animate_ms: u16,
}

impl Style {
    pub(crate) fn apply(&mut self, prop: Prop) {
        match prop {
            Prop::Direction(direction) => self.direction = direction,
            Prop::Width(px) => self.width = Dimension::Px(px),
            Prop::Height(px) => self.height = Dimension::Px(px),
            Prop::WidthFraction(numerator, denominator) => self.width = Dimension::Fraction(numerator, denominator.max(1)),
            Prop::HeightFraction(numerator, denominator) => self.height = Dimension::Fraction(numerator, denominator.max(1)),
            Prop::MinWidth(px) => self.min_width = px,
            Prop::MinHeight(px) => self.min_height = px,
            Prop::MaxWidth(px) => self.max_width = px,
            Prop::MaxHeight(px) => self.max_height = px,
            Prop::Grow(weight) => self.grow = weight,
            Prop::Shrink(weight) => self.shrink = weight,
            Prop::Aspect(width, height) => self.aspect = Some((width, height)),
            Prop::Padding(px) => self.padding = [px; 4],
            Prop::PaddingX(px) => (self.padding[0], self.padding[2]) = (px, px),
            Prop::PaddingY(px) => (self.padding[1], self.padding[3]) = (px, px),
            Prop::PaddingEach(left, top, right, bottom) => self.padding = [left, top, right, bottom],
            Prop::Gap(px) => (self.gap_x, self.gap_y) = (px, px),
            Prop::GapX(px) => self.gap_x = px,
            Prop::GapY(px) => self.gap_y = px,
            Prop::Align(align) => self.align = align,
            Prop::AlignSelf(align) => self.align_self = Some(align),
            Prop::Justify(justify) => self.justify = justify,
            Prop::Left(px) => self.absolute.get_or_insert_default().left = Some(px),
            Prop::Top(px) => self.absolute.get_or_insert_default().top = Some(px),
            Prop::Right(px) => self.absolute.get_or_insert_default().right = Some(px),
            Prop::Bottom(px) => self.absolute.get_or_insert_default().bottom = Some(px),
            Prop::Background(color) => self.background = Some(color),
            Prop::Foreground(color) => self.foreground = Some(color),
            Prop::Radius(px) => self.radius = px,
            Prop::Border(color, width) => (self.border_color, self.border_width) = (Some(color), width),
            Prop::BorderColor(color) => self.border_color = Some(color),
            Prop::BorderWidth(width) => self.border_width = width,
            Prop::BorderTop(color) => self.border_top = Some(color),
            Prop::BorderBottom(color) => self.border_bottom = Some(color),
            Prop::ScrollBorder(id) => self.scroll_border = Some(id),
            Prop::Shadow(y, blur, color) => self.shadow = Some(Shadow { y, blur, color }),
            Prop::Opacity(opacity) => self.opacity = Some(opacity),
            Prop::Clip(clip) => self.clip = clip,
            Prop::Animate(animated, ms) => (self.animated, self.animate_ms) = (animated, ms),
        }
    }

    /// Applies what `state` selects, lightest first and the shared table before
    /// `when` within one weight. Returns whether anything applied.
    pub(crate) fn resolve(&mut self, state: State, when: &[(State, Prop)]) -> bool {
        let shared = self.states.map_or(&[][..], |style| style.states);
        let mut applied = false;
        let mut from = 0u16;
        loop {
            let weights = shared.iter().map(|(s, _)| *s).chain(when.iter().map(|(s, _)| *s));
            let Some(weight) = weights.filter(|s| state.contains(*s) && u16::from(s.bits()) >= from).map(|s| s.bits()).min() else { return applied };
            for prop in shared.iter().filter(|(s, _)| s.bits() == weight).flat_map(|(_, props)| props.iter()) {
                self.apply(*prop);
            }
            for (_, prop) in when.iter().filter(|(s, _)| s.bits() == weight) {
                self.apply(*prop);
            }
            applied = true;
            from = u16::from(weight) + 1;
        }
    }

    pub(crate) fn eases(&self, animated: Animated) -> bool {
        self.animate_ms > 0 && self.animated.contains(animated)
    }

    pub(crate) fn layout_eq(&self, other: &Style) -> bool {
        self.direction == other.direction
            && self.width == other.width
            && self.height == other.height
            && self.min_width == other.min_width
            && self.min_height == other.min_height
            && self.max_width == other.max_width
            && self.max_height == other.max_height
            && self.grow == other.grow
            && self.shrink == other.shrink
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
    pub rotations: Rotations,
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
