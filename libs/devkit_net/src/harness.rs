/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Stacks joined by simulated wires on a simulated clock, for host tests.
 */

use std::time::Duration;

use crate::{IfaceId, Instant, Stack};

pub(crate) type End = (usize, IfaceId);

pub(crate) struct Captured {
    pub at: Instant,
    pub from: End,
    pub frame: Vec<u8>,
}

struct Wire {
    ends: [End; 2],
    delay: Duration,
}

struct InFlight {
    at: Instant,
    to: End,
    frame: Vec<u8>,
}

pub(crate) struct Net {
    pub stacks: Vec<Stack>,
    pub now: Instant,
    pub captured: Vec<Captured>,
    wires: Vec<Wire>,
    in_flight: Vec<InFlight>,
    loss: Option<Box<dyn FnMut(&[u8]) -> bool>>,
    busy: Option<Box<dyn FnMut(&[u8]) -> bool>>,
}

impl Net {
    pub fn new() -> Self {
        Self {
            stacks: Vec::new(),
            now: Instant::ZERO,
            captured: Vec::new(),
            wires: Vec::new(),
            in_flight: Vec::new(),
            loss: None,
            busy: None,
        }
    }

    /// Frames for which `drop` returns true are lost on the wire.
    pub fn set_loss(&mut self, drop: impl FnMut(&[u8]) -> bool + 'static) {
        self.loss = Some(Box::new(drop));
    }

    /// Frames for which `refuse` returns true are refused by the sending link.
    pub fn set_busy(&mut self, refuse: impl FnMut(&[u8]) -> bool + 'static) {
        self.busy = Some(Box::new(refuse));
    }

    pub fn add_stack(&mut self, stack: Stack) -> usize {
        self.stacks.push(stack);
        self.stacks.len() - 1
    }

    /// Brings both interfaces up.
    pub fn connect(&mut self, a: End, b: End, delay: Duration) {
        self.stacks[a.0].set_link_up(a.1, true);
        self.stacks[b.0].set_link_up(b.1, true);
        self.wires.push(Wire { ends: [a, b], delay });
    }

    /// Runs `f` on stack `i`, putting what it transmits on the wires.
    pub fn with<R>(&mut self, i: usize, f: impl FnOnce(&mut Stack, Instant, &mut dyn FnMut(IfaceId, &[u8]) -> bool) -> R) -> R {
        let mut out = Vec::new();
        let wires = &self.wires;
        let busy = &mut self.busy;
        let result = f(&mut self.stacks[i], self.now, &mut |iface, frame| {
            let wired = peer(wires, (i, iface)).is_some() && !busy.as_mut().is_some_and(|refuse| refuse(frame));
            if wired {
                out.push((iface, frame.to_vec()));
            }
            wired
        });
        for (iface, frame) in out {
            self.route((i, iface), frame);
        }
        result
    }

    /// Sends `frame` out of `from` as the stack itself would.
    pub fn send(&mut self, from: End, frame: &[u8]) {
        self.with(from.0, |stack, _, tx| {
            let _ = stack.emit(from.1, frame, tx);
        });
    }

    /// Runs every stack and wire until `deadline`, jumping between events.
    pub fn run_until(&mut self, deadline: Instant) {
        loop {
            let mut next: Option<Instant> = None;
            for i in 0..self.stacks.len() {
                if let Some(at) = self.with(i, |stack, now, tx| stack.poll(now, tx)) {
                    next = Some(next.map_or(at, |n| n.min(at)));
                }
            }
            let now = self.now;
            let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.in_flight).into_iter().partition(|f| f.at <= now);
            self.in_flight = rest;
            if !due.is_empty() {
                for f in due {
                    self.with(f.to.0, |stack, now, tx| stack.receive(f.to.1, &f.frame, now, tx));
                }
                continue;
            }
            for f in &self.in_flight {
                next = Some(next.map_or(f.at, |n| n.min(f.at)));
            }
            match next {
                Some(at) if at <= deadline => self.now = self.now.max(at),
                _ => {
                    self.now = deadline;
                    return;
                }
            }
        }
    }

    pub fn run_for(&mut self, d: Duration) {
        self.run_until(self.now + d);
    }

    fn route(&mut self, from: End, frame: Vec<u8>) {
        let (to, delay) = peer(&self.wires, from).unwrap();
        self.captured.push(Captured { at: self.now, from, frame: frame.clone() });
        if self.loss.as_mut().is_some_and(|drop| drop(&frame)) {
            return;
        }
        self.in_flight.push(InFlight { at: self.now + delay, to, frame });
    }
}

fn peer(wires: &[Wire], end: End) -> Option<(End, Duration)> {
    wires.iter().find_map(|w| match w.ends {
        [a, b] if a == end => Some((b, w.delay)),
        [a, b] if b == end => Some((a, w.delay)),
        _ => None,
    })
}
