/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use super::message::Name;
use crate::time::Instant;

const ENTRIES: usize = 16;
const MAX_TTL: u32 = 24 * 3600;
const MAX_NEGATIVE_TTL: u32 = 300;

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Record {
    Addrs(Vec<Ipv4Addr>),
    Alias(Name),
    Missing,
}

struct Entry {
    name: Name,
    record: Record,
    expires: Instant,
    used: u32,
}

/// Answers by name, for their TTL, cleared whenever the name servers change.
#[derive(Default)]
pub(crate) struct Cache {
    entries: Vec<Entry>,
    servers: u32,
    clock: u32,
}

impl Cache {
    /// `servers` is the stack's name server generation.
    pub fn get(&mut self, name: &[u8], servers: u32, now: Instant) -> Option<Record> {
        self.sync(servers);
        self.entries.retain(|e| e.expires > now);
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.iter_mut().find(|e| e.name == name)?;
        entry.used = self.clock;
        Some(entry.record.clone())
    }

    /// Ignored with a TTL of zero.
    pub fn put(&mut self, name: Name, record: Record, ttl: u32, servers: u32, now: Instant) {
        self.sync(servers);
        let cap = if record == Record::Missing { MAX_NEGATIVE_TTL } else { MAX_TTL };
        let ttl = ttl.min(cap);
        if ttl == 0 {
            return;
        }
        self.entries.retain(|e| e.expires > now && e.name != name);
        if self.entries.len() >= ENTRIES
            && let Some(oldest) = self.entries.iter().enumerate().min_by_key(|(_, e)| e.used).map(|(i, _)| i)
        {
            self.entries.swap_remove(oldest);
        }
        self.clock = self.clock.wrapping_add(1);
        let expires = now + Duration::from_secs(u64::from(ttl));
        self.entries.push(Entry { name, record, expires, used: self.clock });
    }

    fn sync(&mut self, servers: u32) {
        if servers != self.servers {
            self.entries.clear();
            self.servers = servers;
        }
    }
}
