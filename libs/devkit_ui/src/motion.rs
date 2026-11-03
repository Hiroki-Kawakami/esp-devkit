/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::animate::Channel;
use crate::component::Animation;
use crate::nav::Transit;
use crate::node::{NONE, NodeId};
use crate::presence::Running;
use crate::tree::{Fling, Tree};

pub(crate) fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let u = 2.0 - 2.0 * t;
        1.0 - u * u * u / 2.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Curve {
    Linear,
    EaseOut,
    EaseInOut,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Repeat {
    Once,
    Loop,
    /// Every other pass runs backwards.
    PingPong,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    pub(crate) elapsed: u32,
    duration: u32,
    curve: Curve,
    repeat: Repeat,
}

impl Clock {
    pub(crate) fn new(duration_ms: u32, curve: Curve, repeat: Repeat) -> Self {
        Self { elapsed: 0, duration: duration_ms.max(1), curve, repeat }
    }

    pub(crate) fn once(duration_ms: u32) -> Self {
        Self::new(duration_ms, Curve::EaseOut, Repeat::Once)
    }

    /// `true` once a `Once` clock reaches its end; the others never end.
    fn advance(&mut self, dt_ms: u32) -> bool {
        match self.repeat {
            Repeat::Once => {
                self.elapsed = (self.elapsed + dt_ms).min(self.duration);
                self.elapsed == self.duration
            }
            Repeat::Loop => {
                self.elapsed = (self.elapsed + dt_ms) % self.duration;
                false
            }
            Repeat::PingPong => {
                self.elapsed = (self.elapsed + dt_ms) % (2 * self.duration);
                false
            }
        }
    }

    /// Of the pass running now, 0 to 1.
    pub(crate) fn linear(&self) -> f32 {
        let at = if self.repeat == Repeat::PingPong && self.elapsed >= self.duration { self.elapsed - self.duration } else { self.elapsed };
        at as f32 / self.duration as f32
    }

    pub(crate) fn eased(&self) -> f32 {
        let t = self.linear();
        match self.curve {
            Curve::Linear => t,
            Curve::EaseOut => ease_out(t),
            Curve::EaseInOut => ease_in_out(t),
        }
    }

    pub(crate) fn backwards(&self) -> bool {
        self.repeat == Repeat::PingPong && self.elapsed >= self.duration
    }
}

pub(crate) enum MotionKind {
    Transit(Transit),
    Presence(Running),
    Value(Animation),
    Fade(Channel),
    Fling(Fling),
    Phase,
}

/// Something moving on a node by itself, one frame at a time.
pub(crate) struct Motion {
    pub(crate) node: NodeId,
    /// `None` for what decides itself when it rests, as a fling does.
    pub(crate) clock: Option<Clock>,
    pub(crate) kind: MotionKind,
    /// Held until the next `update`: the `tick` after an input counts the idle
    /// time before it, which would finish it at once.
    pub(crate) waiting: bool,
    ended: bool,
}

impl Motion {
    pub(crate) fn new(node: NodeId, clock: Option<Clock>, kind: MotionKind) -> Self {
        Self { node, clock, kind, waiting: false, ended: false }
    }
}

impl Tree {
    pub(crate) fn motion(&self, node: NodeId, which: impl Fn(&MotionKind) -> bool) -> Option<usize> {
        self.motions.iter().position(|m| m.node == node && which(&m.kind))
    }

    pub(crate) fn take_motion(&mut self, i: usize) -> Motion {
        debug_assert!(!self.ticking);
        self.motions.remove(i)
    }

    pub(crate) fn stop_motions(&mut self, node: NodeId, which: impl Fn(&MotionKind) -> bool) {
        self.forget_where(|m| m.node == node && which(&m.kind));
    }

    pub(crate) fn forget_motions(&mut self, node: NodeId) {
        self.forget_where(|m| m.node == node);
        self.ended_motions.retain(|m| m.node != node);
    }

    /// While ticking, indices must stay: what goes is only marked.
    fn forget_where(&mut self, gone: impl Fn(&Motion) -> bool) {
        if self.ticking {
            self.motions.iter_mut().filter(|m| gone(m)).for_each(|m| m.node = NONE);
        } else {
            self.motions.retain(|m| !gone(m));
        }
    }

    pub(crate) fn release_waiting(&mut self) {
        self.motions.iter_mut().for_each(|m| m.waiting = false);
    }

    pub(crate) fn animating(&self) -> bool {
        !self.motions.is_empty()
    }

    pub(crate) fn transit(&self, nav: NodeId) -> Option<&Transit> {
        self.motions.iter().find_map(|m| match &m.kind {
            MotionKind::Transit(transit) if m.node == nav => Some(transit),
            _ => None,
        })
    }

    pub(crate) fn transit_mut(&mut self, nav: NodeId) -> Option<&mut Transit> {
        self.motions.iter_mut().find_map(|m| match &mut m.kind {
            MotionKind::Transit(transit) if m.node == nav => Some(transit),
            _ => None,
        })
    }

    pub(crate) fn transiting(&self) -> bool {
        self.motions.iter().any(|m| matches!(m.kind, MotionKind::Transit(_)))
    }

    /// Moves everything on by `dt_ms`, then ends what got to its end. Ending
    /// one can remove the nodes of others, which then do not end.
    pub(crate) fn tick_motions(&mut self, dt_ms: u32) {
        self.ticking = true;
        let mut i = 0;
        while i < self.motions.len() {
            if self.motions[i].node != NONE && !self.motions[i].waiting {
                let ended = self.step_motion(i, dt_ms);
                self.motions[i].ended = ended;
            }
            i += 1;
        }
        self.ticking = false;
        let (ended, running) = core::mem::take(&mut self.motions).into_iter().filter(|m| m.node != NONE).partition(|m| m.ended);
        self.motions = running;
        self.ended_motions = ended;
        while !self.ended_motions.is_empty() {
            let motion = self.ended_motions.remove(0);
            self.end_motion(motion);
        }
    }

    fn step_motion(&mut self, i: usize, dt_ms: u32) -> bool {
        let motion = &mut self.motions[i];
        let dt_ms = match &mut motion.kind {
            MotionKind::Transit(transit) => if core::mem::take(&mut transit.skip_tick) { 0 } else { dt_ms },
            MotionKind::Presence(running) => {
                // Whoever paints takes the picture before the first tick, or never.
                running.capture = false;
                dt_ms
            }
            _ => dt_ms,
        };
        let ended = motion.clock.as_mut().is_some_and(|clock| clock.advance(dt_ms));
        match motion.kind {
            MotionKind::Transit(_) => self.step_transit(i),
            MotionKind::Presence(_) => self.apply_pose(i),
            MotionKind::Value(_) => self.apply_animation(i),
            MotionKind::Fade(_) => self.step_fade(i),
            MotionKind::Fling(fling) => return self.advance(i, fling, dt_ms),
            MotionKind::Phase => self.step_phase(i),
        }
        ended
    }

    fn end_motion(&mut self, motion: Motion) {
        match motion.kind {
            MotionKind::Transit(transit) => self.end_transit(motion.node, transit),
            MotionKind::Presence(running) => self.end_presence(motion.node, running.appearing),
            MotionKind::Value(animation) => self.end_animation(motion.node, &animation),
            MotionKind::Fade(channel) => self.end_fade(motion.node, channel),
            MotionKind::Fling(_) | MotionKind::Phase => {}
        }
    }
}
