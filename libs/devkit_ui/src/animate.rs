/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use devkit_gfx::Color;

use crate::motion::{Clock, Motion, MotionKind};
use crate::node::NodeIndex;
use crate::style::{Animated, Style};
use crate::tree::Tree;

/// A value on its way from `from` to what the style says now.
#[derive(Clone, Copy)]
pub(crate) struct Fade<T> {
    from: T,
    mix: f32,
}

impl Fade<Color> {
    /// With no `to` it fades out.
    fn color(&self, to: Option<Color>) -> Color {
        self.from.mix(to.unwrap_or(Color { a: 0, ..self.from }), (self.mix * 255.0) as u8)
    }
}

impl Fade<u8> {
    pub(crate) fn opacity(&self, to: u8) -> u8 {
        (f32::from(self.from) + (f32::from(to) - f32::from(self.from)) * self.mix) as u8
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Channel {
    Background,
    Foreground,
    Border,
    Opacity,
}

#[derive(Default)]
pub(crate) struct Fades {
    pub(crate) background: Option<Fade<Color>>,
    pub(crate) foreground: Option<Fade<Color>>,
    pub(crate) border: Option<Fade<Color>>,
    pub(crate) opacity: Option<Fade<u8>>,
}

impl Fades {
    fn is_empty(&self) -> bool {
        self.background.is_none() && self.foreground.is_none() && self.border.is_none() && self.opacity.is_none()
    }

    fn has(&self, channel: Channel) -> bool {
        match channel {
            Channel::Background => self.background.is_some(),
            Channel::Foreground => self.foreground.is_some(),
            Channel::Border => self.border.is_some(),
            Channel::Opacity => self.opacity.is_some(),
        }
    }

    fn set_mix(&mut self, channel: Channel, mix: f32) {
        match channel {
            Channel::Background => self.background.iter_mut().for_each(|f| f.mix = mix),
            Channel::Foreground => self.foreground.iter_mut().for_each(|f| f.mix = mix),
            Channel::Border => self.border.iter_mut().for_each(|f| f.mix = mix),
            Channel::Opacity => self.opacity.iter_mut().for_each(|f| f.mix = mix),
        }
    }

    fn clear(&mut self, channel: Channel) {
        match channel {
            Channel::Background => self.background = None,
            Channel::Foreground => self.foreground = None,
            Channel::Border => self.border = None,
            Channel::Opacity => self.opacity = None,
        }
    }
}

/// What a color headed for `to` shows.
pub(crate) fn faded(fade: Option<&Fade<Color>>, to: Option<Color>) -> Option<Color> {
    fade.map(|fade| fade.color(to)).or(to)
}

fn retarget(slot: &mut Option<Fade<Color>>, from: Option<Color>, to: Option<Color>) {
    let shown = faded(slot.as_ref(), from).or(to.map(|color| Color { a: 0, ..color }));
    *slot = shown.map(|from| Fade { from, mix: 0.0 });
}

impl Tree {
    /// The node's style just changed from `old`. A node not placed yet has
    /// nothing to come from.
    pub(crate) fn ease_style(&mut self, id: NodeIndex, old: &Style) {
        let node = &mut self.nodes[id as usize];
        let style = node.style;
        let background = old.background != style.background;
        let foreground = old.foreground != style.foreground;
        let border = old.border_color != style.border_color;
        let opacity = old.opacity != style.opacity;
        if node.rect.is_empty() || !(background || foreground || border || opacity) {
            return;
        }
        let colors = style.eases(Animated::COLOR);
        let fade_opacity = style.eases(Animated::OPACITY);
        if !colors && !fade_opacity && node.fades.is_none() {
            return;
        }
        let palette = self.slots[usize::from(node.owner)].theme.palette(self.color_mode);
        let resolve = |role: Option<crate::theme::ColorRole>| role.map(|role| palette.color(role));
        let fades = node.fades.get_or_insert_with(|| Box::new_in(Fades::default(), self.caps));
        if background {
            match colors {
                true => retarget(&mut fades.background, resolve(old.background), resolve(style.background)),
                false => fades.background = None,
            }
        }
        if foreground {
            // No color of its own means the inherited one, unknown here.
            match colors && old.foreground.is_some() && style.foreground.is_some() {
                true => retarget(&mut fades.foreground, resolve(old.foreground), resolve(style.foreground)),
                false => fades.foreground = None,
            }
        }
        if border {
            match colors {
                true => retarget(&mut fades.border, resolve(old.border_color), resolve(style.border_color)),
                false => fades.border = None,
            }
        }
        if opacity {
            let was = old.opacity.unwrap_or(255);
            let from = fades.opacity.as_ref().map_or(was, |fade| fade.opacity(was));
            fades.opacity = fade_opacity.then_some(Fade { from, mix: 0.0 });
        }
        let channels = [(background, Channel::Background), (foreground, Channel::Foreground), (border, Channel::Border), (opacity, Channel::Opacity)]
            .map(|(changed, channel)| (changed, channel, fades.has(channel)));
        if fades.is_empty() {
            node.fades = None;
        }
        let waiting = !self.motions.iter().any(|m| m.node == id && matches!(m.kind, MotionKind::Fade(_)) && !m.waiting);
        for (_, channel, has) in channels.into_iter().filter(|c| c.0) {
            self.stop_motions(id, |kind| matches!(kind, MotionKind::Fade(c) if *c == channel));
            if has {
                let mut motion = Motion::new(id, Some(Clock::once(u32::from(style.animate_ms))), MotionKind::Fade(channel));
                motion.waiting = waiting;
                self.motions.push(motion);
            }
        }
    }

    pub(crate) fn step_fade(&mut self, i: usize) {
        let motion = &self.motions[i];
        let (MotionKind::Fade(channel), Some(clock)) = (&motion.kind, motion.clock) else { return };
        let (channel, mix) = (*channel, clock.eased());
        let node = &mut self.nodes[motion.node as usize];
        node.paint_dirty = true;
        if let Some(fades) = &mut node.fades {
            fades.set_mix(channel, mix);
        }
    }

    pub(crate) fn end_fade(&mut self, id: NodeIndex, channel: Channel) {
        let node = &mut self.nodes[id as usize];
        node.paint_dirty = true;
        let Some(fades) = &mut node.fades else { return };
        fades.clear(channel);
        if fades.is_empty() {
            node.fades = None;
        }
    }
}
