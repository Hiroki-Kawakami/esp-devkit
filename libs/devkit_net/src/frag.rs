/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use crate::time::Instant;
use crate::wire::Ipv4;

const MAX_DATAGRAM: usize = 16 * 1024;
const MAX_PENDING: usize = 4;
const TIMEOUT: Duration = Duration::from_secs(30);

struct Pending {
    src: Ipv4Addr,
    dst: Ipv4Addr,
    protocol: u8,
    ident: u16,
    data: Vec<u8>,
    /// Received byte ranges, sorted and merged.
    have: Vec<(usize, usize)>,
    total: Option<usize>,
    deadline: Instant,
}

impl Pending {
    fn complete(&self) -> bool {
        self.total.is_some_and(|total| self.have == [(0, total)])
    }

    fn add_range(&mut self, start: usize, end: usize) {
        let mut merged = (start, end);
        self.have.retain(|&(s, e)| {
            let apart = e < merged.0 || s > merged.1;
            if !apart {
                merged = (merged.0.min(s), merged.1.max(e));
            }
            apart
        });
        let at = self.have.iter().position(|&(s, _)| s > merged.0).unwrap_or(self.have.len());
        self.have.insert(at, merged);
    }
}

/// Datagrams being put back together from their fragments.
#[derive(Default)]
pub(crate) struct Reassembly {
    pending: Vec<Pending>,
}

impl Reassembly {
    /// The whole payload once the last missing fragment arrives. A datagram
    /// over 16 KiB is given up.
    pub fn add(&mut self, ip: &Ipv4, payload: &[u8], now: Instant) -> Option<Vec<u8>> {
        let start = ip.fragment_offset();
        let end = start + payload.len();
        let same = |p: &Pending| p.src == ip.src && p.dst == ip.dst && p.protocol == ip.protocol && p.ident == ip.ident;
        let index = match self.pending.iter().position(same) {
            Some(i) => i,
            None => {
                if self.pending.len() >= MAX_PENDING {
                    let oldest = (0..self.pending.len()).min_by_key(|&i| self.pending[i].deadline).unwrap();
                    self.pending.swap_remove(oldest);
                }
                self.pending.push(Pending {
                    src: ip.src,
                    dst: ip.dst,
                    protocol: ip.protocol,
                    ident: ip.ident,
                    data: Vec::new(),
                    have: Vec::new(),
                    total: None,
                    deadline: now + TIMEOUT,
                });
                self.pending.len() - 1
            }
        };
        let p = &mut self.pending[index];
        if end > MAX_DATAGRAM || p.total.is_some_and(|total| end > total) {
            self.pending.swap_remove(index);
            return None;
        }
        if !ip.more_fragments() {
            p.total = Some(end);
        }
        if p.data.len() < end {
            p.data.resize(end, 0);
        }
        p.data[start..end].copy_from_slice(payload);
        p.add_range(start, end);
        if !p.complete() {
            return None;
        }
        let mut p = self.pending.swap_remove(index);
        p.data.truncate(p.total.unwrap());
        Some(p.data)
    }

    /// Drops what timed out and returns the next deadline.
    pub fn poll(&mut self, now: Instant) -> Option<Instant> {
        self.pending.retain(|p| p.deadline > now);
        self.pending.iter().map(|p| p.deadline).min()
    }
}
