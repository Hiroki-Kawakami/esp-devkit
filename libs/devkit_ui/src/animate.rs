/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use devkit_gfx::Color;

use crate::node::NodeId;
use crate::style::{Animated, Style};
use crate::tree::Tree;

pub(crate) fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

/// A value on its way from `from` to what the style says now.
#[derive(Clone, Copy)]
pub(crate) struct Fade<T> {
    from: T,
    elapsed: u16,
    duration: u16,
}

impl<T> Fade<T> {
    fn eased(&self) -> f32 {
        ease_out(f32::from(self.elapsed) / f32::from(self.duration))
    }

    fn advance(&mut self, dt_ms: u32) -> bool {
        self.elapsed = (u32::from(self.elapsed) + dt_ms).min(u32::from(self.duration)) as u16;
        self.elapsed == self.duration
    }
}

impl Fade<Color> {
    /// With no `to` it fades out.
    fn color(&self, to: Option<Color>) -> Color {
        self.from.mix(to.unwrap_or(Color { a: 0, ..self.from }), (self.eased() * 255.0) as u8)
    }
}

impl Fade<u8> {
    pub(crate) fn opacity(&self, to: u8) -> u8 {
        (f32::from(self.from) + (f32::from(to) - f32::from(self.from)) * self.eased()) as u8
    }
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

    fn advance(&mut self, dt_ms: u32) {
        fn step<T>(slot: &mut Option<Fade<T>>, dt_ms: u32) {
            if slot.as_mut().is_some_and(|fade| fade.advance(dt_ms)) {
                *slot = None;
            }
        }
        step(&mut self.background, dt_ms);
        step(&mut self.foreground, dt_ms);
        step(&mut self.border, dt_ms);
        step(&mut self.opacity, dt_ms);
    }
}

/// What a color headed for `to` shows.
pub(crate) fn faded(fade: Option<&Fade<Color>>, to: Option<Color>) -> Option<Color> {
    fade.map(|fade| fade.color(to)).or(to)
}

fn retarget(slot: &mut Option<Fade<Color>>, from: Option<Color>, to: Option<Color>, duration: u16) {
    let shown = faded(slot.as_ref(), from).or(to.map(|color| Color { a: 0, ..color }));
    *slot = shown.map(|from| Fade { from, elapsed: 0, duration });
}

impl Tree {
    /// The node's style just changed from `old`. A node not placed yet has
    /// nothing to come from.
    pub(crate) fn ease_style(&mut self, id: NodeId, old: &Style) {
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
        let ms = style.animate_ms;
        let fades = node.fades.get_or_insert_with(|| Box::new_in(Fades::default(), self.caps));
        if background {
            match colors {
                true => retarget(&mut fades.background, resolve(old.background), resolve(style.background), ms),
                false => fades.background = None,
            }
        }
        if foreground {
            // No color of its own means the inherited one, unknown here.
            match colors && old.foreground.is_some() && style.foreground.is_some() {
                true => retarget(&mut fades.foreground, resolve(old.foreground), resolve(style.foreground), ms),
                false => fades.foreground = None,
            }
        }
        if border {
            match colors {
                true => retarget(&mut fades.border, resolve(old.border_color), resolve(style.border_color), ms),
                false => fades.border = None,
            }
        }
        if opacity {
            let was = old.opacity.unwrap_or(255);
            let from = fades.opacity.as_ref().map_or(was, |fade| fade.opacity(was));
            fades.opacity = fade_opacity.then_some(Fade { from, elapsed: 0, duration: ms });
        }
        if fades.is_empty() {
            node.fades = None;
        } else if !self.fading.contains(&id) && !self.starting.contains(&id) {
            self.starting.push(id);
        }
    }

    pub(crate) fn tick_fades(&mut self, dt_ms: u32) {
        let mut i = 0;
        while let Some(&id) = self.fading.get(i) {
            let node = &mut self.nodes[id as usize];
            node.paint_dirty = true;
            if let Some(fades) = &mut node.fades {
                fades.advance(dt_ms);
            }
            if node.fades.as_ref().is_none_or(|fades| fades.is_empty()) {
                node.fades = None;
                self.fading.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    pub(crate) fn forget_fades(&mut self, id: NodeId) {
        self.nodes[id as usize].fades = None;
        self.fading.retain(|f| *f != id);
        self.starting.retain(|f| *f != id);
    }
}
