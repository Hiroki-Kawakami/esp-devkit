/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::net::{Ipv4Addr, SocketAddrV4};

use idf::heap::Caps;

use crate::icmp::Reported;
use crate::ring::Ring;
use crate::stack::{IfaceId, SocketId, Stack, Tx};
use crate::time::Instant;
use crate::wire::{Ipv4, PROTO_UDP, UDP_HEADER, be16, checksum, put16, sum};
use crate::{Error, Result};

const EPHEMERAL_FIRST: u16 = 49152;
const MAX_QUEUED: usize = 64;

pub(crate) struct Socket {
    local: SocketAddrV4,
    iface: Option<IfaceId>,
    remote: Option<SocketAddrV4>,
    rx: Ring,
    queue: VecDeque<(SocketAddrV4, usize)>,
    error: Option<Error>,
}

#[derive(Default)]
pub(crate) struct Sockets {
    slots: Vec<Option<Socket>>,
}

impl Sockets {
    fn get(&mut self, id: usize) -> &mut Socket {
        self.slots[id].as_mut().unwrap()
    }

    fn in_use(&self, addr: SocketAddrV4, iface: Option<IfaceId>) -> bool {
        self.slots.iter().flatten().any(|s| {
            s.local.port() == addr.port()
                && (s.local.ip().is_unspecified() || addr.ip().is_unspecified() || s.local.ip() == addr.ip())
                && (s.iface.is_none() || iface.is_none() || s.iface == iface)
        })
    }

    /// RFC 6056: from a random point of the range.
    fn ephemeral(&mut self) -> Result<u16> {
        let span = u32::from(u16::MAX - EPHEMERAL_FIRST) + 1;
        let start = idf::system::random() % span;
        (0..span)
            .map(|i| EPHEMERAL_FIRST + ((start + i) % span) as u16)
            .find(|&port| !self.in_use(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port), None))
            .ok_or(Error::AddrInUse)
    }
}

fn pseudo_sum(src: Ipv4Addr, dst: Ipv4Addr, len: usize) -> u32 {
    let acc = sum(&src.octets(), 0);
    let acc = sum(&dst.octets(), acc);
    acc + u32::from(PROTO_UDP) + len as u32
}

impl Stack {
    /// A socket on `iface` sends and receives only through it, also while the
    /// interface has no address.
    pub(crate) fn udp_bind(
        &mut self,
        addr: SocketAddrV4,
        iface: Option<IfaceId>,
        rx_size: usize,
        caps: Caps,
    ) -> Result<usize> {
        if !addr.ip().is_unspecified() && !self.is_own_address(*addr.ip()) {
            return Err(Error::AddrNotAvailable);
        }
        let port = if addr.port() == 0 {
            self.udp.ephemeral()?
        } else if self.udp.in_use(addr, iface) {
            return Err(Error::AddrInUse);
        } else {
            addr.port()
        };
        let socket = Socket {
            local: SocketAddrV4::new(*addr.ip(), port),
            iface,
            remote: None,
            rx: Ring::new(rx_size, caps),
            queue: VecDeque::new(),
            error: None,
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

    #[cfg_attr(CONFIG_DEVKIT_NET_IRAM_OPT, unsafe(link_section = ".iram1.devkit_net"))]
    pub(crate) fn udp_send(&mut self, id: usize, dst: SocketAddrV4, data: &[u8], now: Instant, tx: &mut Tx) -> Result<()> {
        let socket = self.udp.get(id);
        let (local, iface) = (socket.local, socket.iface);
        let mut src = (!local.ip().is_unspecified()).then_some(*local.ip());
        let (via, next_hop) = match iface {
            Some(iface) => {
                if self.ipv4(iface).is_none() {
                    src = Some(Ipv4Addr::UNSPECIFIED);
                }
                (iface, self.next_hop_on(iface, *dst.ip()).ok_or(Error::NoRoute)?)
            }
            None => self.route_v4(*dst.ip()).ok_or(Error::NoRoute)?,
        };
        let len = UDP_HEADER + data.len();
        self.send_ipv4_on(via, next_hop, src, *dst.ip(), PROTO_UDP, len, now, tx, |out, src| {
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

    /// Copies the oldest datagram into `buf`, dropping what does not fit. An
    /// ICMP error about the connected peer is returned once, first.
    pub(crate) fn udp_recv(&mut self, id: usize, buf: &mut [u8]) -> Option<Result<(usize, SocketAddrV4)>> {
        let socket = self.udp.get(id);
        if let Some(error) = socket.error.take() {
            return Some(Err(error));
        }
        let (from, len) = socket.queue.pop_front()?;
        Some(Ok((socket.rx.pop(len, crate::ring::uninit(buf)), from)))
    }

    /// Only connected sockets hear of errors (RFC 1122 4.1.3.3 leaves the
    /// others to time out).
    pub(crate) fn udp_icmp_error(&mut self, local: SocketAddrV4, remote: SocketAddrV4, reported: Reported) {
        let found = self.udp.slots.iter().position(|s| {
            s.as_ref().is_some_and(|s| {
                s.local.port() == local.port()
                    && (s.local.ip().is_unspecified() || s.local.ip() == local.ip())
                    && s.remote == Some(remote)
            })
        });
        if let Some(id) = found {
            self.udp.get(id).error = Some(reported.into());
            if !self.woken.contains(&SocketId::Udp(id)) {
                self.woken.push(SocketId::Udp(id));
            }
        }
    }

    /// `false` when no socket is bound to the destination port.
    #[cfg_attr(CONFIG_DEVKIT_NET_IRAM_OPT, unsafe(link_section = ".iram1.devkit_net"))]
    pub(crate) fn udp_input(&mut self, arrived: IfaceId, ip: &Ipv4, p: &[u8]) -> bool {
        if p.len() < UDP_HEADER {
            return true;
        }
        let len = usize::from(be16(p, 4));
        if len < UDP_HEADER || len > p.len() {
            return true;
        }
        let p = &p[..len];
        if be16(p, 6) != 0 && checksum(sum(p, pseudo_sum(ip.src, ip.dst, len))) != 0 {
            return true;
        }
        let from = SocketAddrV4::new(ip.src, be16(p, 0));
        let port = be16(p, 2);
        let Some(id) = self.udp.slots.iter().position(|s| {
            s.as_ref().is_some_and(|s| {
                s.local.port() == port
                    && (s.local.ip().is_unspecified() || *s.local.ip() == ip.dst)
                    && s.iface.is_none_or(|i| i == arrived)
                    && s.remote.is_none_or(|r| r == from)
            })
        }) else {
            return false;
        };
        let data = &p[UDP_HEADER..];
        let socket = self.udp.get(id);
        if socket.rx.free() < data.len() || socket.queue.len() >= MAX_QUEUED {
            return true;
        }
        if !socket.rx.write_free(0, data) {
            return true;
        }
        socket.rx.commit(data.len());
        socket.queue.push_back((from, data.len()));
        if !self.woken.contains(&SocketId::Udp(id)) {
            self.woken.push(SocketId::Udp(id));
        }
        true
    }
}
