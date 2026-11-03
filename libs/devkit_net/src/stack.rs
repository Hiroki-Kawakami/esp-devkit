/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::link::{LinkInfo, Medium};
use crate::time::Instant;

const ETHERNET_HEADER: usize = 14;
const LOOPBACK_MTU: usize = 16384;
const LOOPBACK_QUEUE: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct IfaceId(u8);

impl IfaceId {
    pub const LOOPBACK: IfaceId = IfaceId(0);
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct IfaceStats {
    pub rx_frames: u32,
    pub rx_dropped: u32,
    pub tx_frames: u32,
    pub tx_dropped: u32,
}

struct Iface {
    name: &'static str,
    info: LinkInfo,
    up: bool,
    stats: IfaceStats,
}

impl Iface {
    fn header_len(&self) -> usize {
        match self.info.medium {
            Medium::Ethernet => ETHERNET_HEADER,
            Medium::Ip => 0,
        }
    }
}

/// The protocol state of every interface, driven by `receive` and `poll`.
/// Performs no I/O and reads no clock of its own.
pub struct Stack {
    ifaces: Vec<Iface>,
    loopback: VecDeque<Vec<u8>>,
}

impl Default for Stack {
    fn default() -> Self {
        Self::new()
    }
}

impl Stack {
    pub fn new() -> Self {
        let mut stack = Self { ifaces: Vec::new(), loopback: VecDeque::new() };
        let lo = stack.add_interface("lo", LinkInfo { medium: Medium::Ip, mac: [0; 6], mtu: LOOPBACK_MTU });
        stack.set_link_up(lo, true);
        stack
    }

    /// The interface starts down.
    pub fn add_interface(&mut self, name: &'static str, info: LinkInfo) -> IfaceId {
        let id = IfaceId(u8::try_from(self.ifaces.len()).expect("too many interfaces"));
        self.ifaces.push(Iface { name, info, up: false, stats: IfaceStats::default() });
        id
    }

    pub fn set_link_up(&mut self, id: IfaceId, up: bool) {
        self.iface_mut(id).up = up;
    }

    pub fn is_link_up(&self, id: IfaceId) -> bool {
        self.iface(id).up
    }

    pub fn name(&self, id: IfaceId) -> &'static str {
        self.iface(id).name
    }

    pub fn link_info(&self, id: IfaceId) -> LinkInfo {
        self.iface(id).info
    }

    pub fn stats(&self, id: IfaceId) -> IfaceStats {
        self.iface(id).stats
    }

    pub fn receive(&mut self, id: IfaceId, frame: &[u8], _now: Instant) {
        let iface = self.iface_mut(id);
        let header = iface.header_len();
        if !iface.up || frame.len() <= header || frame.len() > header + iface.info.mtu {
            iface.stats.rx_dropped += 1;
            return;
        }
        iface.stats.rx_frames += 1;
    }

    /// Hands what is due by `now` to `tx`, which returns whether the link
    /// took the frame. Returns when to poll next; `None` waits for the next
    /// `receive` or call on the stack.
    pub fn poll(&mut self, now: Instant, _tx: &mut dyn FnMut(IfaceId, &[u8]) -> bool) -> Option<Instant> {
        for _ in 0..self.loopback.len() {
            let Some(frame) = self.loopback.pop_front() else { break };
            self.receive(IfaceId::LOOPBACK, &frame, now);
        }
        if self.loopback.is_empty() { None } else { Some(now) }
    }

    pub(crate) fn emit(&mut self, id: IfaceId, frame: &[u8], tx: &mut dyn FnMut(IfaceId, &[u8]) -> bool) {
        let loopback_full = self.loopback.len() >= LOOPBACK_QUEUE;
        let iface = self.iface_mut(id);
        let sent = if !iface.up {
            false
        } else if id == IfaceId::LOOPBACK {
            !loopback_full
        } else {
            tx(id, frame)
        };
        if !sent {
            iface.stats.tx_dropped += 1;
            return;
        }
        iface.stats.tx_frames += 1;
        if id == IfaceId::LOOPBACK {
            self.loopback.push_back(frame.to_vec());
        }
    }

    fn iface(&self, id: IfaceId) -> &Iface {
        &self.ifaces[usize::from(id.0)]
    }

    fn iface_mut(&mut self, id: IfaceId) -> &mut Iface {
        &mut self.ifaces[usize::from(id.0)]
    }
}
