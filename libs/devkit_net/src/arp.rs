/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use crate::stack::IfaceId;
use crate::time::Instant;
use crate::wire::Mac;

const CAPACITY: usize = 16;
pub(crate) const RETRY: Duration = Duration::from_secs(1);
pub(crate) const MAX_REQUESTS: u8 = 3;
pub(crate) const LIFETIME: Duration = Duration::from_secs(300);
/// An entry in use is asked about again this long before it expires.
const REFRESH: Duration = Duration::from_secs(30);

enum State {
    Resolving { next: Instant, sent: u8 },
    Resolved { mac: Mac, expires: Instant, refreshing: bool },
}

struct Entry {
    iface: IfaceId,
    ip: Ipv4Addr,
    state: State,
    pending: Option<Vec<u8>>,
}

pub(crate) enum Resolve {
    Known(Mac),
    /// Known, but ask `Mac` for itself again so the entry does not lapse.
    Refresh(Mac),
    /// Send a request now.
    Request,
    Waiting,
}

#[derive(Default)]
pub(crate) struct Cache {
    entries: Vec<Entry>,
}

impl Cache {
    /// On a miss, keeps `packet` (the latest one only) until the reply.
    pub fn resolve(&mut self, iface: IfaceId, ip: Ipv4Addr, now: Instant, packet: &[u8]) -> Resolve {
        if let Some(e) = self.find(iface, ip) {
            return match &mut e.state {
                State::Resolved { mac, expires, refreshing } if *expires > now => {
                    if *expires - now < REFRESH && !*refreshing {
                        *refreshing = true;
                        Resolve::Refresh(*mac)
                    } else {
                        Resolve::Known(*mac)
                    }
                }
                State::Resolved { .. } => {
                    e.state = State::Resolving { next: now + RETRY, sent: 1 };
                    e.pending = Some(packet.to_vec());
                    Resolve::Request
                }
                State::Resolving { .. } => {
                    e.pending = Some(packet.to_vec());
                    Resolve::Waiting
                }
            };
        }
        self.make_room();
        self.entries.push(Entry {
            iface,
            ip,
            state: State::Resolving { next: now + RETRY, sent: 1 },
            pending: Some(packet.to_vec()),
        });
        Resolve::Request
    }

    /// Records `ip` at `mac` if it is known already or `create`; returns the
    /// packet that waited for it.
    pub fn update(&mut self, iface: IfaceId, ip: Ipv4Addr, mac: Mac, now: Instant, create: bool) -> Option<Vec<u8>> {
        let state = State::Resolved { mac, expires: now + LIFETIME, refreshing: false };
        if let Some(e) = self.find(iface, ip) {
            e.state = state;
            return e.pending.take();
        }
        if create {
            self.make_room();
            self.entries.push(Entry { iface, ip, state, pending: None });
        }
        None
    }

    /// Calls `request` for each address to ask again, drops those that never
    /// answered, and returns the next deadline.
    pub fn poll(&mut self, now: Instant, mut request: impl FnMut(IfaceId, Ipv4Addr)) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        self.entries.retain_mut(|e| {
            let due = match &mut e.state {
                State::Resolving { next, sent } => {
                    if *next <= now {
                        if *sent >= MAX_REQUESTS {
                            return false;
                        }
                        *sent += 1;
                        *next = now + RETRY;
                        request(e.iface, e.ip);
                    }
                    *next
                }
                State::Resolved { expires, .. } => {
                    if *expires <= now {
                        return false;
                    }
                    *expires
                }
            };
            next = Some(next.map_or(due, |n| n.min(due)));
            true
        });
        next
    }

    pub fn forget_iface(&mut self, iface: IfaceId) {
        self.entries.retain(|e| e.iface != iface);
    }

    fn find(&mut self, iface: IfaceId, ip: Ipv4Addr) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.iface == iface && e.ip == ip)
    }

    fn make_room(&mut self) {
        if self.entries.len() < CAPACITY {
            return;
        }
        let expires = |e: &Entry| match e.state {
            State::Resolved { expires, .. } => expires,
            State::Resolving { .. } => Instant::from_micros(u64::MAX),
        };
        let oldest = (0..self.entries.len()).min_by_key(|&i| expires(&self.entries[i])).unwrap();
        self.entries.swap_remove(oldest);
    }
}
