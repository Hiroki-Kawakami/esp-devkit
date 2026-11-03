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
}

impl Net {
    pub fn new() -> Self {
        Self { stacks: Vec::new(), now: Instant::ZERO, captured: Vec::new(), wires: Vec::new(), in_flight: Vec::new() }
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

    /// Sends `frame` out of `from` as the stack itself would.
    pub fn send(&mut self, from: End, frame: &[u8]) {
        let mut out = Vec::new();
        let wires = &self.wires;
        self.stacks[from.0].emit(from.1, frame, &mut |iface, frame| {
            let wired = peer(wires, (from.0, iface)).is_some();
            if wired {
                out.push((iface, frame.to_vec()));
            }
            wired
        });
        for (iface, frame) in out {
            self.route((from.0, iface), frame);
        }
    }

    /// Runs every stack and wire until `deadline`, jumping between events.
    pub fn run_until(&mut self, deadline: Instant) {
        loop {
            let mut next = self.poll_all();
            let due: Vec<InFlight> = self.take_due();
            if !due.is_empty() {
                for f in due {
                    self.stacks[f.to.0].receive(f.to.1, &f.frame, self.now);
                }
                continue;
            }
            for f in &self.in_flight {
                next = Some(next.map_or(f.at, |n: Instant| n.min(f.at)));
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

    fn poll_all(&mut self) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        for i in 0..self.stacks.len() {
            let mut out = Vec::new();
            let wires = &self.wires;
            let at = self.stacks[i].poll(self.now, &mut |iface, frame| {
                let wired = peer(wires, (i, iface)).is_some();
                if wired {
                    out.push((iface, frame.to_vec()));
                }
                wired
            });
            if let Some(at) = at {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
            for (iface, frame) in out {
                self.route((i, iface), frame);
            }
        }
        next
    }

    fn route(&mut self, from: End, frame: Vec<u8>) {
        let (to, delay) = peer(&self.wires, from).unwrap();
        self.captured.push(Captured { at: self.now, from, frame: frame.clone() });
        self.in_flight.push(InFlight { at: self.now + delay, to, frame });
    }

    fn take_due(&mut self) -> Vec<InFlight> {
        let now = self.now;
        let (due, rest) = std::mem::take(&mut self.in_flight).into_iter().partition(|f| f.at <= now);
        self.in_flight = rest;
        due
    }
}

fn peer(wires: &[Wire], end: End) -> Option<(End, Duration)> {
    wires.iter().find_map(|w| match w.ends {
        [a, b] if a == end => Some((b, w.delay)),
        [a, b] if b == end => Some((a, w.delay)),
        _ => None,
    })
}
