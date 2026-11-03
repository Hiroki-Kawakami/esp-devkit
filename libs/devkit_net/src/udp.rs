/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::net::{Ipv4Addr, SocketAddrV4};

use idf::heap::Caps;

use crate::ring::Ring;
use crate::stack::{SocketId, Stack, Tx};
use crate::time::Instant;
use crate::wire::{Ipv4, PROTO_UDP, UDP_HEADER, be16, checksum, put16, sum};
use crate::{Error, Result};

const EPHEMERAL_FIRST: u16 = 49152;
const MAX_QUEUED: usize = 64;

pub(crate) struct Socket {
    local: SocketAddrV4,
    remote: Option<SocketAddrV4>,
    rx: Ring,
    queue: VecDeque<(SocketAddrV4, usize)>,
}

#[derive(Default)]
pub(crate) struct Sockets {
    slots: Vec<Option<Socket>>,
    next_port: u16,
}

impl Sockets {
    fn get(&mut self, id: usize) -> &mut Socket {
        self.slots[id].as_mut().unwrap()
    }

    fn in_use(&self, addr: SocketAddrV4) -> bool {
        self.slots.iter().flatten().any(|s| {
            s.local.port() == addr.port()
                && (s.local.ip().is_unspecified() || addr.ip().is_unspecified() || s.local.ip() == addr.ip())
        })
    }

    fn ephemeral(&mut self) -> Result<u16> {
        let span = u16::MAX - EPHEMERAL_FIRST + 1;
        for _ in 0..span {
            let port = EPHEMERAL_FIRST + self.next_port % span;
            self.next_port = self.next_port.wrapping_add(1);
            if !self.in_use(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)) {
                return Ok(port);
            }
        }
        Err(Error::AddrInUse)
    }
}

fn pseudo_sum(src: Ipv4Addr, dst: Ipv4Addr, len: usize) -> u32 {
    let acc = sum(&src.octets(), 0);
    let acc = sum(&dst.octets(), acc);
    acc + u32::from(PROTO_UDP) + len as u32
}

impl Stack {
    pub(crate) fn udp_bind(&mut self, addr: SocketAddrV4, rx_size: usize, caps: Caps) -> Result<usize> {
        if !addr.ip().is_unspecified() && !self.is_own_address(*addr.ip()) {
            return Err(Error::AddrNotAvailable);
        }
        let port = if addr.port() == 0 {
            self.udp.ephemeral()?
        } else if self.udp.in_use(addr) {
            return Err(Error::AddrInUse);
        } else {
            addr.port()
        };
        let socket = Socket {
            local: SocketAddrV4::new(*addr.ip(), port),
            remote: None,
            rx: Ring::new(rx_size, caps)?,
            queue: VecDeque::new(),
        };
        let slots = &mut self.udp.slots;
        match slots.iter().position(Option::is_none) {
            Some(id) => {
                slots[id] = Some(socket);
                Ok(id)
            }
            None => {
                slots.push(Some(socket));
                Ok(slots.len() - 1)
            }
        }
    }

    pub(crate) fn udp_close(&mut self, id: usize) {
        self.udp.slots[id] = None;
        self.woken.retain(|w| *w != SocketId::Udp(id));
    }

    /// Only datagrams from `remote` are received from then on.
    pub(crate) fn udp_connect(&mut self, id: usize, remote: SocketAddrV4) {
        self.udp.get(id).remote = Some(remote);
    }

    pub(crate) fn udp_local(&mut self, id: usize) -> SocketAddrV4 {
        self.udp.get(id).local
    }

    pub(crate) fn udp_peer(&mut self, id: usize) -> Option<SocketAddrV4> {
        self.udp.get(id).remote
    }

    pub(crate) fn udp_send(&mut self, id: usize, dst: SocketAddrV4, data: &[u8], now: Instant, tx: &mut Tx) -> Result<()> {
        let local = self.udp.get(id).local;
        if UDP_HEADER + data.len() > self.mtu_to(*dst.ip()).ok_or(Error::NoRoute)? {
            return Err(Error::MessageTooLarge);
        }
        let src = (!local.ip().is_unspecified()).then_some(*local.ip());
        let len = UDP_HEADER + data.len();
        self.send_ipv4(src, *dst.ip(), PROTO_UDP, len, now, tx, |out, src| {
            put16(out, 0, local.port());
            put16(out, 2, dst.port());
            put16(out, 4, len as u16);
            put16(out, 6, 0);
            out[UDP_HEADER..].copy_from_slice(data);
            let c = match checksum(sum(out, pseudo_sum(src, *dst.ip(), len))) {
                0 => 0xffff,
                c => c,
            };
            put16(out, 6, c);
        })
    }

    /// Copies the oldest datagram into `buf`, dropping what does not fit.
    pub(crate) fn udp_recv(&mut self, id: usize, buf: &mut [u8]) -> Option<(usize, SocketAddrV4)> {
        let socket = self.udp.get(id);
        let (from, len) = socket.queue.pop_front()?;
        Some((socket.rx.pop(len, buf), from))
    }

    pub(crate) fn udp_input(&mut self, ip: &Ipv4, p: &[u8]) {
        if p.len() < UDP_HEADER {
            return;
        }
        let len = usize::from(be16(p, 4));
        if len < UDP_HEADER || len > p.len() {
            return;
        }
        let p = &p[..len];
        if be16(p, 6) != 0 && checksum(sum(p, pseudo_sum(ip.src, ip.dst, len))) != 0 {
            return;
        }
        let from = SocketAddrV4::new(ip.src, be16(p, 0));
        let port = be16(p, 2);
        let Some(id) = self.udp.slots.iter().position(|s| {
            s.as_ref().is_some_and(|s| {
                s.local.port() == port
                    && (s.local.ip().is_unspecified() || *s.local.ip() == ip.dst)
                    && s.remote.is_none_or(|r| r == from)
            })
        }) else {
            return;
        };
        let data = &p[UDP_HEADER..];
        let socket = self.udp.get(id);
        if socket.rx.free() < data.len() || socket.queue.len() >= MAX_QUEUED {
            return;
        }
        socket.rx.push(data);
        socket.queue.push_back((from, data.len()));
        if !self.woken.contains(&SocketId::Udp(id)) {
            self.woken.push(SocketId::Udp(id));
        }
    }
}
