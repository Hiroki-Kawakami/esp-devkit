/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::{Color, DisplayList, Rect};
use idf::heap::Caps;

use crate::motion::{Clock, Curve, Motion, MotionKind, Repeat};
use crate::node::{NodeContent, NodeId};
use crate::paint::{Effects, dim};
use crate::theme::{ColorRole, Palette};
use crate::tree::Tree;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Gauge {
    /// Not knowing how far, a segment of `segment` percent of the width goes
    /// across and back.
    Bar { segment: u8 },
    /// Not knowing how far, an arc of `sweep` degrees goes round.
    Ring { stroke: u8, sweep: u16, track: ColorRole, fill: ColorRole },
}

pub(crate) struct ProgressNode {
    pub(crate) gauge: Gauge,
    pub(crate) value: Option<f32>,
    pub(crate) period_ms: u16,
    pub(crate) phase: f32,
    pub(crate) backwards: bool,
}

impl ProgressNode {
    pub(crate) fn new(gauge: Gauge, value: Option<f32>, period_ms: u16) -> Self {
        Self { gauge, value, period_ms, phase: 0.0, backwards: false }
    }

    fn moves(&self) -> bool {
        self.value.is_none() && self.period_ms > 0
    }
}

impl Tree {
    pub(crate) fn sync_progress(&mut self, id: NodeId) {
        let NodeContent::Progress(progress) = &self.nodes[id as usize].content else { return };
        let (gauge, period, moves) = (progress.gauge, u32::from(progress.period_ms), progress.moves());
        let running = self.motion(id, |kind| matches!(kind, MotionKind::Phase)).is_some();
        if moves == running {
            return;
        }
        if !moves {
            self.stop_motions(id, |kind| matches!(kind, MotionKind::Phase));
            return;
        }
        let clock = match gauge {
            Gauge::Bar { .. } => Clock::new(period, Curve::EaseInOut, Repeat::PingPong),
            Gauge::Ring { .. } => Clock::new(period, Curve::Linear, Repeat::Loop),
        };
        self.motions.push(Motion::new(id, Some(clock), MotionKind::Phase));
    }

    pub(crate) fn step_phase(&mut self, i: usize) {
        let motion = &self.motions[i];
        let Some(clock) = motion.clock else { return };
        let node = &mut self.nodes[motion.node as usize];
        if let NodeContent::Progress(progress) = &mut node.content {
            (progress.phase, progress.backwards) = (clock.eased(), clock.backwards());
            node.paint_dirty = true;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_progress(&self, progress: &ProgressNode, rect: Rect, radius: u8, foreground: Option<Color>, opacity: u8, palette: &Palette, effects: &mut Effects, list: &mut DisplayList<'_, Caps>) {
        match progress.gauge {
            Gauge::Bar { segment } => {
                let color = dim(foreground.unwrap_or(palette.primary), opacity);
                let radius = i16::from(radius).min(rect.h / 2).min(rect.w / 2);
                let corner = (radius > 0).then(|| effects.corner(radius as u8));
                match corner {
                    Some(corner) => list.push_round_clip(rect, corner),
                    None => list.push_clip(rect),
                }
                match progress.value {
                    Some(value) => {
                        let filled = (f32::from(rect.w) * value.clamp(0.0, 1.0)) as i16;
                        // Whole length, so a short fill keeps its round end.
                        let bar = Rect::new(rect.x + filled - rect.w, rect.y, rect.w, rect.h);
                        match corner {
                            Some(corner) if filled > 0 => list.round_rect(bar, corner, color),
                            None if filled > 0 => list.fill(bar, color),
                            _ => {}
                        }
                    }
                    None => {
                        let length = (i32::from(rect.w) * i32::from(segment) / 100) as i16;
                        let length = length.max(rect.h).min(rect.w);
                        let (t, backwards) = if progress.period_ms == 0 { (0.5, false) } else { (progress.phase, progress.backwards) };
                        let travel = (f32::from(rect.w + length) * t) as i16;
                        let left = if backwards { rect.w - travel } else { travel - length };
                        let clear = Color { a: 0, ..color };
                        let (from, to) = if backwards { (color, clear) } else { (clear, color) };
                        list.gradient(Rect::new(rect.x + left, rect.y, length, rect.h), corner, from, to, false);
                    }
                }
                match corner {
                    Some(_) => list.pop_round_clip(),
                    None => list.pop_clip(),
                }
            }
            Gauge::Ring { stroke, sweep, track, fill } => {
                let radius = rect.w.min(rect.h) / 2;
                if radius <= 0 {
                    return;
                }
                let (x, y) = (rect.x + rect.w / 2 - radius, rect.y + rect.h / 2 - radius);
                let mask = effects.arc(radius as u16, u16::from(stroke));
                let track = dim(palette.color(track), opacity);
                let fill = dim(palette.color(fill), opacity);
                list.arc(x, y, mask, 0.0, 360.0, track, track);
                match progress.value {
                    Some(value) if value > 0.0 => list.arc(x, y, mask, 0.0, 360.0 * value.min(1.0), fill, fill),
                    Some(_) => {}
                    None => list.arc(x, y, mask, progress.phase * 360.0, f32::from(sweep), Color { a: 0, ..fill }, fill),
                }
            }
        }
    }
}
