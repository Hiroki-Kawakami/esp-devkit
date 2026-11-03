/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::mem;
use core::net::Ipv4Addr;

use crate::Error;
use crate::arp::{self, Resolve};
use crate::link::{LinkInfo, Medium};
use crate::time::Instant;
use crate::{tcp, udp};
use crate::wire::{
    ARP_LEN, ARP_REPLY, ARP_REQUEST, Arp, BROADCAST_MAC, DEFAULT_TTL, ETHERNET_HEADER, ETHERNET_MIN_FRAME,
    ETHERTYPE_ARP, ETHERTYPE_IPV4, Ethernet, ICMP_ECHO_REPLY, ICMP_ECHO_REQUEST, IPV4_HEADER, Ipv4, Mac, PROTO_ICMP, PROTO_TCP, PROTO_UDP,
    checksum, put16, sum,
};

const LOOPBACK_MTU: usize = 16384;
const LOOPBACK_QUEUE: usize = 16;

pub(crate) type Tx<'a> = dyn FnMut(IfaceId, &[u8]) -> bool + 'a;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct IfaceId(u8);

impl IfaceId {
    pub const LOOPBACK: IfaceId = IfaceId(0);

    pub(crate) fn index(self) -> usize {
        usize::from(self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SocketId {
    Udp(usize),
    Tcp(usize),
    Listener(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct IfaceStats {
    pub rx_frames: u32,
    pub rx_dropped: u32,
    pub tx_frames: u32,
    pub tx_dropped: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ipv4Config {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Option<Ipv4Addr>,
}

impl Ipv4Config {
    fn mask(&self) -> u32 {
        u32::MAX.checked_shl(32 - u32::from(self.prefix_len)).unwrap_or(0)
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        (ip.to_bits() ^ self.address.to_bits()) & self.mask() == 0
    }

    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from_bits(self.address.to_bits() | !self.mask())
    }
}

struct Iface {
    name: &'static str,
    info: LinkInfo,
    up: bool,
    ipv4: Option<Ipv4Config>,
    dns: Vec<Ipv4Addr>,
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
/// Performs no I/O and reads no clock of its own; frames go out through the
/// `tx` passed to either, which returns whether the link took the frame.
pub struct Stack {
    ifaces: Vec<Iface>,
    loopback: VecDeque<Vec<u8>>,
    arp: arp::Cache,
    buf: Vec<u8>,
    ident: u16,
    pub(crate) udp: udp::Sockets,
    pub(crate) tcp: tcp::Sockets,
    pub(crate) woken: Vec<SocketId>,
}

impl Default for Stack {
    fn default() -> Self {
        Self::new()
    }
}

impl Stack {
    pub fn new() -> Self {
        let mut stack = Self {
            ifaces: Vec::new(),
            loopback: VecDeque::new(),
            arp: arp::Cache::default(),
            buf: Vec::new(),
            ident: 0,
            udp: udp::Sockets::default(),
            tcp: tcp::Sockets::default(),
            woken: Vec::new(),
        };
        let lo = stack.add_interface("lo", LinkInfo { medium: Medium::Ip, mac: [0; 6], mtu: LOOPBACK_MTU });
        stack.set_ipv4(lo, Some(Ipv4Config { address: Ipv4Addr::LOCALHOST, prefix_len: 8, gateway: None }));
        stack.set_link_up(lo, true);
        stack
    }

    /// The interface starts down, without an address.
    pub fn add_interface(&mut self, name: &'static str, info: LinkInfo) -> IfaceId {
        let id = IfaceId(u8::try_from(self.ifaces.len()).expect("too many interfaces"));
        self.ifaces.push(Iface { name, info, up: false, ipv4: None, dns: Vec::new(), stats: IfaceStats::default() });
        id
    }

    pub fn set_link_up(&mut self, id: IfaceId, up: bool) {
        self.iface_mut(id).up = up;
        if !up {
            self.arp.forget_iface(id);
        }
    }

    pub fn is_link_up(&self, id: IfaceId) -> bool {
        self.iface(id).up
    }

    pub fn set_ipv4(&mut self, id: IfaceId, config: Option<Ipv4Config>) {
        assert!(config.is_none_or(|c| c.prefix_len <= 32));
        self.iface_mut(id).ipv4 = config;
        self.arp.forget_iface(id);
    }

    pub fn ipv4(&self, id: IfaceId) -> Option<Ipv4Config> {
        self.iface(id).ipv4
    }

    pub fn set_dns_servers(&mut self, id: IfaceId, servers: &[Ipv4Addr]) {
        let dns = &mut self.iface_mut(id).dns;
        dns.clear();
        dns.extend_from_slice(servers);
    }

    pub fn dns_servers(&self, id: IfaceId) -> &[Ipv4Addr] {
        &self.iface(id).dns
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

    pub fn receive(&mut self, id: IfaceId, frame: &[u8], now: Instant, tx: &mut Tx) {
        let iface = self.iface_mut(id);
        let header = iface.header_len();
        if !iface.up || frame.len() <= header || frame.len() > header + iface.info.mtu {
            iface.stats.rx_dropped += 1;
            return;
        }
        iface.stats.rx_frames += 1;
        match iface.info.medium {
            Medium::Ip => self.ipv4_input(id, frame, now, tx),
            Medium::Ethernet => {
                let mac = iface.info.mac;
                let Some(eth) = Ethernet::parse(frame) else { return };
                if eth.src == mac || (eth.dst != mac && eth.dst != BROADCAST_MAC) {
                    return;
                }
                match eth.ethertype {
                    ETHERTYPE_ARP => self.arp_input(id, eth.payload, now, tx),
                    ETHERTYPE_IPV4 => self.ipv4_input(id, eth.payload, now, tx),
                    _ => {}
                }
            }
        }
    }

    /// Sends what is due by `now` and returns when to poll next; `None` waits
    /// for the next `receive` or call on the stack.
    pub fn poll(&mut self, now: Instant, tx: &mut Tx) -> Option<Instant> {
        for _ in 0..self.loopback.len() {
            let Some(frame) = self.loopback.pop_front() else { break };
            self.receive(IfaceId::LOOPBACK, &frame, now, tx);
        }
        let mut requests = Vec::new();
        let arp = self.arp.poll(now, |id, ip| requests.push((id, ip)));
        for (id, ip) in requests {
            self.send_arp_request(id, ip, tx);
        }
        let tcp = self.tcp_poll(now, tx);
        let next = [arp, tcp].into_iter().flatten().min();
        if self.loopback.is_empty() { next } else { Some(now) }
    }

    pub(crate) fn send_ipv4(
        &mut self,
        src: Option<Ipv4Addr>,
        dst: Ipv4Addr,
        protocol: u8,
        len: usize,
        now: Instant,
        tx: &mut Tx,
        fill: impl FnOnce(&mut [u8], Ipv4Addr),
    ) -> Result<(), Error> {
        let (id, next_hop) = self.route_v4(dst).ok_or(Error::NoRoute)?;
        self.send_ipv4_on(id, next_hop, src, dst, protocol, len, now, tx, fill)
    }

    /// The next hop toward `dst` out of interface `id`.
    pub(crate) fn next_hop_on(&self, id: IfaceId, dst: Ipv4Addr) -> Option<Ipv4Addr> {
        if dst.is_broadcast() {
            return Some(dst);
        }
        let config = self.iface(id).ipv4?;
        if config.contains(dst) { Some(dst) } else { config.gateway }
    }

    /// `src` defaults to the interface's address.
    pub(crate) fn send_ipv4_on(
        &mut self,
        id: IfaceId,
        next_hop: Ipv4Addr,
        src: Option<Ipv4Addr>,
        dst: Ipv4Addr,
        protocol: u8,
        len: usize,
        now: Instant,
        tx: &mut Tx,
        fill: impl FnOnce(&mut [u8], Ipv4Addr),
    ) -> Result<(), Error> {
        let iface = self.iface(id);
        let src = match src {
            Some(src) => src,
            None => iface.ipv4.ok_or(Error::NoRoute)?.address,
        };
        if IPV4_HEADER + len > iface.info.mtu {
            return Err(Error::MessageTooLarge);
        }
        let header = iface.header_len();
        let medium = iface.info.medium;
        let broadcast = next_hop.is_broadcast() || iface.ipv4.is_some_and(|c| next_hop == c.broadcast());

        let mut buf = mem::take(&mut self.buf);
        buf.clear();
        buf.resize(header + IPV4_HEADER + len, 0);
        self.ident = self.ident.wrapping_add(1);
        let ip = Ipv4 { src, dst, protocol, ttl: DEFAULT_TTL, ident: self.ident, fragmented: false };
        ip.write_header(&mut buf[header..], len);
        fill(&mut buf[header + IPV4_HEADER..], src);
        match medium {
            Medium::Ip => self.emit(id, &buf, tx),
            Medium::Ethernet if broadcast => self.emit_ethernet(id, BROADCAST_MAC, ETHERTYPE_IPV4, &mut buf, tx),
            Medium::Ethernet => match self.arp.resolve(id, next_hop, now, &buf[header..]) {
                Resolve::Known(mac) => self.emit_ethernet(id, mac, ETHERTYPE_IPV4, &mut buf, tx),
                Resolve::Request => self.send_arp_request(id, next_hop, tx),
                Resolve::Waiting => {}
            },
        }
        self.buf = buf;
        Ok(())
    }

    fn arp_input(&mut self, id: IfaceId, p: &[u8], now: Instant, tx: &mut Tx) {
        let Some(arp) = Arp::parse(p) else { return };
        let iface = self.iface(id);
        let Some(config) = iface.ipv4 else { return };
        let mac = iface.info.mac;
        let for_us = arp.tpa == config.address;
        if !arp.spa.is_unspecified()
            && let Some(packet) = self.arp.update(id, arp.spa, arp.sha, now, for_us)
        {
            let mut buf = mem::take(&mut self.buf);
            buf.clear();
            buf.resize(ETHERNET_HEADER, 0);
            buf.extend_from_slice(&packet);
            self.emit_ethernet(id, arp.sha, ETHERTYPE_IPV4, &mut buf, tx);
            self.buf = buf;
        }
        if for_us && arp.op == ARP_REQUEST {
            let reply = Arp { op: ARP_REPLY, sha: mac, spa: config.address, tha: arp.sha, tpa: arp.spa };
            self.send_arp(id, arp.sha, &reply, tx);
        }
    }

    fn send_arp_request(&mut self, id: IfaceId, ip: Ipv4Addr, tx: &mut Tx) {
        let iface = self.iface(id);
        let spa = iface.ipv4.map_or(Ipv4Addr::UNSPECIFIED, |c| c.address);
        let request = Arp { op: ARP_REQUEST, sha: iface.info.mac, spa, tha: [0; 6], tpa: ip };
        self.send_arp(id, BROADCAST_MAC, &request, tx);
    }

    fn send_arp(&mut self, id: IfaceId, dst: Mac, arp: &Arp, tx: &mut Tx) {
        let mut buf = mem::take(&mut self.buf);
        buf.clear();
        buf.resize(ETHERNET_HEADER + ARP_LEN, 0);
        arp.write(&mut buf[ETHERNET_HEADER..]);
        self.emit_ethernet(id, dst, ETHERTYPE_ARP, &mut buf, tx);
        self.buf = buf;
    }

    /// `buf` holds the payload after room for the header.
    fn emit_ethernet(&mut self, id: IfaceId, dst: Mac, ethertype: u16, buf: &mut Vec<u8>, tx: &mut Tx) {
        Ethernet::write_header(buf, dst, self.iface(id).info.mac, ethertype);
        if buf.len() < ETHERNET_MIN_FRAME {
            buf.resize(ETHERNET_MIN_FRAME, 0);
        }
        self.emit(id, buf, tx);
    }

    fn ipv4_input(&mut self, id: IfaceId, p: &[u8], now: Instant, tx: &mut Tx) {
        let Some((ip, payload)) = Ipv4::parse(p) else { return };
        if ip.fragmented {
            return;
        }
        let Some(config) = self.iface(id).ipv4 else {
            if ip.protocol == PROTO_UDP {
                self.udp_input(id, &ip, payload);
            }
            return;
        };
        let unicast = if id == IfaceId::LOOPBACK {
            ip.dst.is_loopback() || self.is_own_address(ip.dst)
        } else {
            ip.dst == config.address
        };
        if !unicast && !ip.dst.is_broadcast() && ip.dst != config.broadcast() {
            return;
        }
        match ip.protocol {
            PROTO_ICMP => self.icmp_input(&ip, unicast, payload, now, tx),
            PROTO_UDP => self.udp_input(id, &ip, payload),
            PROTO_TCP => self.tcp_input(&ip, unicast, payload, now, tx),
            _ => {}
        }
    }

    fn icmp_input(&mut self, ip: &Ipv4, unicast: bool, p: &[u8], now: Instant, tx: &mut Tx) {
        if p.len() < 8 || checksum(sum(p, 0)) != 0 {
            return;
        }
        if p[0] == ICMP_ECHO_REQUEST && p[1] == 0 && unicast {
            let _ = self.send_ipv4(Some(ip.dst), ip.src, PROTO_ICMP, p.len(), now, tx, |out, _| {
                out.copy_from_slice(p);
                out[0] = ICMP_ECHO_REPLY;
                put16(out, 2, 0);
                let c = checksum(sum(out, 0));
                put16(out, 2, c);
            });
        }
    }

    /// The largest IPv4 payload toward `dst`.
    pub(crate) fn mtu_to(&self, dst: Ipv4Addr) -> Option<usize> {
        self.route_v4(dst).map(|(id, _)| self.iface(id).info.mtu - IPV4_HEADER)
    }

    pub(crate) fn is_own_address(&self, ip: Ipv4Addr) -> bool {
        self.ifaces.iter().any(|i| i.ipv4.is_some_and(|c| c.address == ip))
    }

    /// The interface and next hop for `dst`.
    pub(crate) fn route_v4(&self, dst: Ipv4Addr) -> Option<(IfaceId, Ipv4Addr)> {
        if dst.is_loopback() || self.is_own_address(dst) {
            return Some((IfaceId::LOOPBACK, dst));
        }
        let configured = || {
            self.ifaces
                .iter()
                .enumerate()
                .skip(1)
                .filter(|(_, i)| i.up)
                .filter_map(|(n, i)| i.ipv4.map(|c| (IfaceId(n as u8), c)))
        };
        if let Some((id, _)) = configured().find(|(_, c)| dst.is_broadcast() || c.contains(dst)) {
            return Some((id, dst));
        }
        configured().find_map(|(id, c)| c.gateway.map(|gateway| (id, gateway)))
    }

    pub(crate) fn emit(&mut self, id: IfaceId, frame: &[u8], tx: &mut Tx) {
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
