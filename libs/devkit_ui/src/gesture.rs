/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::erased::Erased;
use crate::node::{NONE, NodeId};
use crate::tree::{Contact, TAP_SLOP, Tree};

const DOUBLE_TAP_MS: u32 = 300;
const DOUBLE_TAP_REACH: i32 = 40;
const LONG_PRESS_MS: u32 = 500;
const SWIPE_MS: u32 = 400;
const SWIPE_MIN: i32 = 64;

devkit::bitflags! {
    /// What a node recognizes, reported through one handler.
    #[derive(Default)]
    pub struct Gestures: u8 {
        const PRESS = 1 << 0;
        const PAN = 1 << 1;
        const PINCH = 1 << 2;
        const ROTATE = 1 << 3;
        const DOUBLE_TAP = 1 << 4;
        const LONG_PRESS = 1 << 5;
        const SWIPE = 1 << 6;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Began,
    Changed,
    Ended,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Coordinates are the node's. `Changed` comes at most once per frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Gesture {
    /// The first finger landed. Comes whatever else is recognized.
    Down { x: i16, y: i16 },
    /// The last finger lifted.
    Up { x: i16, y: i16 },
    /// `dx` and `dy` count from where the finger landed.
    Pan { phase: Phase, x: i16, y: i16, dx: i16, dy: i16 },
    /// `scale` counts from where the fingers landed; `x` and `y` are between them.
    Pinch { phase: Phase, scale: f32, x: i16, y: i16 },
    /// `angle` in radians, clockwise, counts from where the fingers landed.
    Rotate { phase: Phase, angle: f32, x: i16, y: i16 },
    DoubleTap { x: i16, y: i16 },
    LongPress { x: i16, y: i16 },
    Swipe { direction: Direction },
}

#[derive(Clone, Copy)]
pub(crate) struct GestureMap {
    pub(crate) gestures: Gestures,
    pub(crate) function: *const (),
    pub(crate) call: unsafe fn(*const (), Gesture) -> Erased,
}

pub(crate) unsafe fn call_gesture_map<M: Copy>(function: *const (), gesture: Gesture) -> Erased {
    let function: fn(Gesture) -> M = unsafe { core::mem::transmute(function) };
    Erased::new(function(gesture))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Single,
    Panning,
    Multi,
    Done,
}

#[derive(Clone, Copy)]
struct Waiting {
    point: (i16, i16),
    left_ms: u32,
    tap: bool,
}

/// One node's fingers. It outlives them while a first tap waits for a second.
pub(crate) struct Tracker {
    node: NodeId,
    fingers: u8,
    tracks: [u8; 2],
    mode: Mode,
    start: (i16, i16),
    start_ms: u32,
    point: (i16, i16),
    held_ms: u32,
    moved: bool,
    vector: (i32, i32),
    span: i32,
    angle: f32,
    changed: [Option<Gesture>; 2],
    waiting: Option<Waiting>,
}

fn distance(a: (i16, i16), b: (i16, i16)) -> i32 {
    let (dx, dy) = (i32::from(a.0) - i32::from(b.0), i32::from(a.1) - i32::from(b.1));
    ((dx * dx + dy * dy) as u32).isqrt() as i32
}

// Within a tenth of a degree. `core` has no `atan2`, and a call into libm for
// this would bring in `double` arithmetic.
fn atan2(y: f32, x: f32) -> f32 {
    use core::f32::consts::{FRAC_PI_2, PI};
    let (ax, ay) = (x.abs(), y.abs());
    if ax == 0.0 && ay == 0.0 {
        return 0.0;
    }
    let a = ax.min(ay) / ax.max(ay);
    let s = a * a;
    let r = ((-0.046_496_475 * s + 0.159_314_22) * s - 0.327_622_76) * s * a + a;
    let r = if ay > ax { FRAC_PI_2 - r } else { r };
    let r = if x < 0.0 { PI - r } else { r };
    if y < 0.0 { -r } else { r }
}

impl Tree {
    fn tracker_of(&self, node: NodeId) -> Option<usize> {
        self.trackers.iter().position(|t| t.node == node)
    }

    pub(crate) fn gestures_of(&self, node: NodeId) -> Gestures {
        self.nodes[node as usize].gestures.map_or(Gestures::empty(), |g| g.gestures)
    }

    fn local(&self, node: NodeId, point: (i16, i16)) -> (i16, i16) {
        let screen = self.nodes[node as usize].screen;
        (point.0 - screen.x, point.1 - screen.y)
    }

    fn emit(&mut self, node: NodeId, gesture: Gesture) {
        let node = &self.nodes[node as usize];
        let Some(map) = node.gestures else { return };
        let msg = unsafe { (map.call)(map.function, gesture) };
        self.queue.push_back((node.owner, msg));
    }

    fn flush(&mut self, index: usize) {
        let node = self.trackers[index].node;
        for slot in 0..2 {
            if let Some(gesture) = self.trackers[index].changed[slot].take() {
                self.emit(node, gesture);
            }
        }
    }

    pub(crate) fn flush_gestures(&mut self) {
        for index in 0..self.trackers.len() {
            self.flush(index);
        }
    }

    pub(crate) fn node_holds_fingers(&self, node: NodeId) -> bool {
        self.tracker_of(node).is_some_and(|i| self.trackers[i].fingers > 0)
    }

    fn pinch_events(&mut self, index: usize, phase: Phase) -> [Option<Gesture>; 2] {
        let tracker = &self.trackers[index];
        let node = tracker.node;
        let track = |t: u8| self.contacts.iter().find(|c| c.track == t).map_or(tracker.point, |c| c.last);
        let (a, b) = (track(tracker.tracks[0]), track(tracker.tracks[1]));
        let (x, y) = self.local(node, ((a.0 + b.0) / 2, (a.1 + b.1) / 2));
        let vector = (i32::from(b.0) - i32::from(a.0), i32::from(b.1) - i32::from(a.1));
        let (from, span) = (tracker.vector, tracker.span);
        let cross = from.0 * vector.1 - from.1 * vector.0;
        let dot = from.0 * vector.0 + from.1 * vector.1;
        let angle = tracker.angle + atan2(cross as f32, dot as f32);
        let scale = distance(a, b) as f32 / span as f32;
        let gestures = self.gestures_of(node);
        let tracker = &mut self.trackers[index];
        tracker.vector = vector;
        tracker.angle = angle;
        [
            gestures.contains(Gestures::PINCH).then_some(Gesture::Pinch { phase, scale, x, y }),
            gestures.contains(Gestures::ROTATE).then_some(Gesture::Rotate { phase, angle, x, y }),
        ]
    }

    /// A second finger landing on a node that takes two. It stops whatever the
    /// first finger was doing there.
    pub(crate) fn join_gesture(&mut self, node: NodeId, track: u8, point: (i16, i16)) -> bool {
        let gestures = self.gestures_of(node);
        let Some(index) = self.tracker_of(node) else { return false };
        let tracker = &self.trackers[index];
        if tracker.fingers != 1 || !(gestures.contains(Gestures::PINCH) || gestures.contains(Gestures::ROTATE)) {
            return false;
        }
        let first = tracker.tracks[0];
        let panning = tracker.mode == Mode::Panning;
        self.flush(index);
        let Some(contact) = self.contacts.iter_mut().find(|c| c.track == first) else { return false };
        let from = contact.last;
        let tap = contact.tap;
        contact.tap = NONE;
        contact.scroller = NONE;
        contact.dragging = false;
        contact.passive = true;
        self.set_pressed(tap, false);
        if panning {
            let dx = from.0 - self.trackers[index].start.0;
            let dy = from.1 - self.trackers[index].start.1;
            let (x, y) = self.local(node, from);
            self.emit(node, Gesture::Pan { phase: Phase::Ended, x, y, dx, dy });
        }
        let vector = (i32::from(point.0) - i32::from(from.0), i32::from(point.1) - i32::from(from.1));
        let tracker = &mut self.trackers[index];
        tracker.fingers = 2;
        tracker.tracks[1] = track;
        tracker.mode = Mode::Multi;
        tracker.waiting = None;
        tracker.vector = vector;
        tracker.span = distance(from, point).max(1);
        tracker.angle = 0.0;
        tracker.point = point;
        true
    }

    /// Reports the start of a pinch once its second contact is in place.
    pub(crate) fn gesture_joined(&mut self, node: NodeId) {
        let Some(index) = self.tracker_of(node) else { return };
        for gesture in self.pinch_events(index, Phase::Began).into_iter().flatten() {
            self.emit(node, gesture);
        }
    }

    pub(crate) fn gesture_pressed(&mut self, node: NodeId, track: u8, point: (i16, i16), now_ms: u32) {
        if node == NONE {
            return;
        }
        let index = self.tracker_of(node).unwrap_or_else(|| {
            self.trackers.push(Tracker {
                node,
                fingers: 0,
                tracks: [0; 2],
                mode: Mode::Single,
                start: point,
                start_ms: now_ms,
                point,
                held_ms: 0,
                moved: false,
                vector: (0, 0),
                span: 1,
                angle: 0.0,
                changed: [None; 2],
                waiting: None,
            });
            self.trackers.len() - 1
        });
        let tracker = &mut self.trackers[index];
        tracker.fingers = 1;
        tracker.tracks[0] = track;
        tracker.mode = Mode::Single;
        tracker.start = point;
        tracker.start_ms = now_ms;
        tracker.point = point;
        tracker.held_ms = 0;
        tracker.moved = false;
        if self.gestures_of(node).contains(Gestures::PRESS) {
            let (x, y) = self.local(node, point);
            self.emit(node, Gesture::Down { x, y });
        }
    }

    pub(crate) fn gesture_moved(&mut self, contact_index: usize) {
        let contact = self.contacts[contact_index];
        if contact.gesture == NONE {
            return;
        }
        let node = contact.gesture;
        let Some(index) = self.tracker_of(node) else { return };
        let gestures = self.gestures_of(node);
        let (x, y) = self.local(node, contact.last);
        let tracker = &mut self.trackers[index];
        let (dx, dy) = (contact.last.0 - tracker.start.0, contact.last.1 - tracker.start.1);
        match tracker.mode {
            Mode::Multi => {
                let [a, b] = self.pinch_events(index, Phase::Changed);
                self.trackers[index].changed = [a, b];
            }
            Mode::Panning => tracker.changed[0] = Some(Gesture::Pan { phase: Phase::Changed, x, y, dx, dy }),
            Mode::Single => {
                tracker.point = contact.last;
                if tracker.moved || distance(tracker.start, contact.last) <= TAP_SLOP {
                    return;
                }
                tracker.moved = true;
                let pans = gestures.contains(Gestures::PAN) && contact.pan;
                if pans {
                    tracker.mode = Mode::Panning;
                }
                if pans || gestures.contains(Gestures::SWIPE) {
                    self.set_pressed(contact.tap, false);
                    self.contacts[contact_index].tap = NONE;
                }
                if pans {
                    self.contacts[contact_index].passive = true;
                    self.emit(node, Gesture::Pan { phase: Phase::Began, x, y, dx, dy });
                }
            }
            Mode::Done => {}
        }
    }

    /// `true` when the lift meant something here and is not a tap.
    pub(crate) fn gesture_released(&mut self, contact: &Contact, now_ms: u32) -> bool {
        let node = contact.gesture;
        if node == NONE {
            return false;
        }
        let Some(index) = self.tracker_of(node) else { return false };
        self.flush(index);
        let gestures = self.gestures_of(node);
        let (x, y) = self.local(node, contact.last);
        self.trackers[index].fingers -= 1;
        let fingers = self.trackers[index].fingers;
        if fingers == 0 && gestures.contains(Gestures::PRESS) {
            self.emit(node, Gesture::Up { x, y });
        }
        let tracker = &self.trackers[index];
        let (start, start_ms, moved) = (tracker.start, tracker.start_ms, tracker.moved);
        let mut consumed = false;
        match tracker.mode {
            Mode::Multi => {
                self.trackers[index].point = contact.last;
                for gesture in self.pinch_events(index, Phase::Ended).into_iter().flatten() {
                    self.emit(node, gesture);
                }
                self.trackers[index].mode = Mode::Done;
                consumed = true;
            }
            Mode::Panning => {
                let (dx, dy) = (contact.last.0 - start.0, contact.last.1 - start.1);
                self.emit(node, Gesture::Pan { phase: Phase::Ended, x, y, dx, dy });
                consumed = true;
            }
            Mode::Single if !moved => {
                let own_tap = contact.tap == NONE || contact.tap == node;
                if gestures.contains(Gestures::DOUBLE_TAP) && own_tap {
                    consumed = true;
                    self.second_tap(index, contact);
                }
            }
            Mode::Single => {}
            Mode::Done => consumed = true,
        }
        if moved && !contact.dragging && gestures.contains(Gestures::SWIPE) && now_ms.wrapping_sub(start_ms) <= SWIPE_MS {
            let (dx, dy) = (i32::from(contact.last.0) - i32::from(start.0), i32::from(contact.last.1) - i32::from(start.1));
            let direction = match (dx.abs() > dy.abs(), dx > 0, dy > 0) {
                (true, true, _) => Direction::Right,
                (true, false, _) => Direction::Left,
                (false, _, true) => Direction::Down,
                (false, _, false) => Direction::Up,
            };
            if dx.abs().max(dy.abs()) >= SWIPE_MIN {
                self.emit(node, Gesture::Swipe { direction });
                consumed = true;
            }
        }
        if fingers == 0 && self.trackers[index].waiting.is_none() {
            self.trackers.swap_remove(index);
        }
        consumed
    }

    fn second_tap(&mut self, index: usize, contact: &Contact) {
        let node = self.trackers[index].node;
        let first = self.trackers[index].waiting.take();
        if let Some(first) = first {
            if distance(first.point, contact.last) <= DOUBLE_TAP_REACH {
                let (x, y) = self.local(node, contact.last);
                self.emit(node, Gesture::DoubleTap { x, y });
                return;
            }
            if first.tap {
                self.finish_tap(node, first.point, false);
            }
        }
        self.trackers[index].waiting = Some(Waiting { point: contact.last, left_ms: DOUBLE_TAP_MS, tap: contact.tap == node });
    }

    pub(crate) fn gesture_timed(&self) -> bool {
        self.trackers.iter().any(|t| {
            t.waiting.is_some() || t.fingers == 1 && t.mode == Mode::Single && !t.moved && self.gestures_of(t.node).contains(Gestures::LONG_PRESS)
        })
    }

    pub(crate) fn tick_gestures(&mut self, dt_ms: u32) {
        let mut index = 0;
        while index < self.trackers.len() {
            let node = self.trackers[index].node;
            let tracker = &mut self.trackers[index];
            let holding = tracker.fingers == 1 && tracker.mode == Mode::Single && !tracker.moved;
            let point = tracker.point;
            if holding && self.gestures_of(node).contains(Gestures::LONG_PRESS) {
                let tracker = &mut self.trackers[index];
                tracker.held_ms += dt_ms;
                if tracker.held_ms >= LONG_PRESS_MS {
                    tracker.mode = Mode::Done;
                    tracker.waiting = None;
                    let (x, y) = self.local(node, point);
                    self.emit(node, Gesture::LongPress { x, y });
                    if let Some(contact) = self.contacts.iter_mut().find(|c| c.gesture == node) {
                        let tap = contact.tap;
                        contact.tap = NONE;
                        contact.passive = true;
                        self.set_pressed(tap, false);
                    }
                }
            }
            let tracker = &mut self.trackers[index];
            if let Some(waiting) = &mut tracker.waiting {
                waiting.left_ms = waiting.left_ms.saturating_sub(dt_ms);
                if waiting.left_ms == 0 {
                    let waiting = *waiting;
                    tracker.waiting = None;
                    if waiting.tap {
                        self.finish_tap(node, waiting.point, false);
                    }
                }
            }
            let tracker = &self.trackers[index];
            if tracker.fingers == 0 && tracker.waiting.is_none() {
                self.trackers.swap_remove(index);
            } else {
                index += 1;
            }
        }
    }

    pub(crate) fn forget_gestures(&mut self, node: NodeId) {
        self.trackers.retain(|t| t.node != node);
        for contact in &mut self.contacts {
            if contact.gesture == node {
                contact.gesture = NONE;
            }
        }
    }
}
