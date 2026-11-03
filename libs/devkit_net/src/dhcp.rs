/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use core::time::Duration;

use idf::executor;
use idf::time::Instant;

use crate::net::Net;
use crate::stack::IfaceId;
use crate::wire::{Mac, be16};
use crate::{Ipv4Config, UdpBuilder, UdpSocket};

pub(crate) const SERVER_PORT: u16 = 67;
pub(crate) const CLIENT_PORT: u16 = 68;
const MAGIC: [u8; 4] = [99, 130, 83, 99];
const FIXED: usize = 236;
const BROADCAST_FLAG: u16 = 0x8000;

pub(crate) const DISCOVER: u8 = 1;
pub(crate) const OFFER: u8 = 2;
pub(crate) const REQUEST: u8 = 3;
pub(crate) const ACK: u8 = 5;
pub(crate) const NAK: u8 = 6;

const OPT_PAD: u8 = 0;
const OPT_SUBNET: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_REQUESTED: u8 = 50;
const OPT_LEASE: u8 = 51;
const OPT_TYPE: u8 = 53;
const OPT_SERVER: u8 = 54;
const OPT_PARAMS: u8 = 55;
const OPT_T1: u8 = 58;
const OPT_T2: u8 = 59;
const OPT_CLIENT_ID: u8 = 61;
const OPT_END: u8 = 255;

const MAX_DNS: usize = 3;
const FIRST_RETRY: Duration = Duration::from_secs(4);
const MAX_RETRY: Duration = Duration::from_secs(64);
const MIN_RENEW_RETRY: Duration = Duration::from_secs(60);
const LONGEST_SLEEP: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Message {
    pub op: u8,
    pub xid: u32,
    pub flags: u16,
    pub ciaddr: Ipv4Addr,
    pub yiaddr: Ipv4Addr,
    pub chaddr: Mac,
    pub kind: u8,
    pub server: Option<Ipv4Addr>,
    pub requested: Option<Ipv4Addr>,
    pub subnet: Option<Ipv4Addr>,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub lease: Option<u32>,
    pub t1: Option<u32>,
    pub t2: Option<u32>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            op: 0,
            xid: 0,
            flags: 0,
            ciaddr: Ipv4Addr::UNSPECIFIED,
            yiaddr: Ipv4Addr::UNSPECIFIED,
            chaddr: [0; 6],
            kind: 0,
            server: None,
            requested: None,
            subnet: None,
            router: None,
            dns: Vec::new(),
            lease: None,
            t1: None,
            t2: None,
        }
    }
}

fn addr_at(p: &[u8], at: usize) -> Ipv4Addr {
    Ipv4Addr::new(p[at], p[at + 1], p[at + 2], p[at + 3])
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        let mut p = alloc::vec![0u8; FIXED];
        p[0] = self.op;
        p[1] = 1;
        p[2] = 6;
        p[4..8].copy_from_slice(&self.xid.to_be_bytes());
        p[10..12].copy_from_slice(&self.flags.to_be_bytes());
        p[12..16].copy_from_slice(&self.ciaddr.octets());
        p[16..20].copy_from_slice(&self.yiaddr.octets());
        p[28..34].copy_from_slice(&self.chaddr);
        p.extend_from_slice(&MAGIC);
        p.extend_from_slice(&[OPT_TYPE, 1, self.kind]);
        let mut addr = |code: u8, a: Option<Ipv4Addr>| {
            if let Some(a) = a {
                p.extend_from_slice(&[code, 4]);
                p.extend_from_slice(&a.octets());
            }
        };
        addr(OPT_SERVER, self.server);
        addr(OPT_REQUESTED, self.requested);
        addr(OPT_SUBNET, self.subnet);
        addr(OPT_ROUTER, self.router);
        if !self.dns.is_empty() {
            p.extend_from_slice(&[OPT_DNS, (self.dns.len() * 4) as u8]);
            for a in &self.dns {
                p.extend_from_slice(&a.octets());
            }
        }
        for (code, value) in [(OPT_LEASE, self.lease), (OPT_T1, self.t1), (OPT_T2, self.t2)] {
            if let Some(v) = value {
                p.extend_from_slice(&[code, 4]);
                p.extend_from_slice(&v.to_be_bytes());
            }
        }
        if self.op == 1 {
            p.extend_from_slice(&[OPT_PARAMS, 4, OPT_SUBNET, OPT_ROUTER, OPT_DNS, OPT_LEASE]);
            p.extend_from_slice(&[OPT_CLIENT_ID, 7, 1]);
            p.extend_from_slice(&self.chaddr);
        }
        p.push(OPT_END);
        p
    }

    pub fn decode(p: &[u8]) -> Option<Self> {
        if p.len() < FIXED + MAGIC.len() || p[1] != 1 || p[2] != 6 || p[FIXED..FIXED + 4] != MAGIC {
            return None;
        }
        let mut m = Self {
            op: p[0],
            xid: u32::from_be_bytes(p[4..8].try_into().unwrap()),
            flags: be16(p, 10),
            ciaddr: addr_at(p, 12),
            yiaddr: addr_at(p, 16),
            chaddr: p[28..34].try_into().unwrap(),
            ..Self::default()
        };
        let mut opts = &p[FIXED + 4..];
        while let [code, rest @ ..] = opts {
            match *code {
                OPT_PAD => {
                    opts = rest;
                    continue;
                }
                OPT_END => break,
                _ => {}
            }
            let (&len, rest) = rest.split_first()?;
            let value = rest.get(..usize::from(len))?;
            let addr = || (value.len() >= 4).then(|| addr_at(value, 0));
            let secs = || value.try_into().ok().map(u32::from_be_bytes);
            match *code {
                OPT_TYPE => m.kind = *value.first()?,
                OPT_SERVER => m.server = addr(),
                OPT_REQUESTED => m.requested = addr(),
                OPT_SUBNET => m.subnet = addr(),
                OPT_ROUTER => m.router = addr(),
                OPT_DNS => m.dns = value.chunks_exact(4).take(MAX_DNS).map(|c| addr_at(c, 0)).collect(),
                OPT_LEASE => m.lease = secs(),
                OPT_T1 => m.t1 = secs(),
                OPT_T2 => m.t2 = secs(),
                _ => {}
            }
            opts = &rest[usize::from(len)..];
        }
        (m.kind != 0).then_some(m)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Lease {
    pub config: Ipv4Config,
    pub dns: Vec<Ipv4Addr>,
    pub server: Ipv4Addr,
    pub lease: Duration,
    pub t1: Duration,
    pub t2: Duration,
}

impl Lease {
    fn from_ack(ack: &Message) -> Option<Self> {
        if ack.yiaddr.is_unspecified() {
            return None;
        }
        let prefix_len = ack.subnet.map_or(24, |m| m.to_bits().leading_ones() as u8);
        let lease = u64::from(ack.lease?);
        Some(Self {
            config: Ipv4Config { address: ack.yiaddr, prefix_len, gateway: ack.router },
            dns: ack.dns.clone(),
            server: ack.server?,
            lease: Duration::from_secs(lease),
            t1: Duration::from_secs(ack.t1.map_or(lease / 2, u64::from)),
            t2: Duration::from_secs(ack.t2.map_or(lease * 7 / 8, u64::from)),
        })
    }
}

struct Client {
    net: &'static Net,
    iface: IfaceId,
    mac: Mac,
    socket: UdpSocket,
}

fn broadcast() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, SERVER_PORT))
}

async fn sleep(d: Duration) {
    let mut left = d;
    while !left.is_zero() {
        let step = left.min(LONGEST_SLEEP);
        executor::sleep(step).await;
        left -= step;
    }
}

impl Client {
    fn request(&self, kind: u8, xid: u32) -> Message {
        Message { op: 1, xid, flags: BROADCAST_FLAG, chaddr: self.mac, kind, ..Message::default() }
    }

    fn send(&self, m: &Message, to: SocketAddr) {
        let _ = self.socket.send_to(&m.encode(), to);
    }

    /// The first reply to `xid` of one of `kinds` within `timeout`.
    async fn reply(&self, xid: u32, kinds: &[u8], timeout: Duration) -> Option<Message> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 576];
        loop {
            let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
            let (n, _) = executor::timeout(left, self.socket.recv_from_async(&mut buf)).await.ok()?.ok()?;
            if let Some(m) = Message::decode(&buf[..n])
                && m.op == 2
                && m.xid == xid
                && m.chaddr == self.mac
                && kinds.contains(&m.kind)
            {
                return Some(m);
            }
        }
    }

    async fn acquire(&self) -> Lease {
        let mut retry = FIRST_RETRY;
        loop {
            let xid = idf::system::random();
            self.send(&self.request(DISCOVER, xid), broadcast());
            if let Some(offer) = self.reply(xid, &[OFFER], retry).await
                && let Some(lease) = self.select(&offer).await
            {
                return lease;
            }
            retry = (retry * 2).min(MAX_RETRY);
        }
    }

    async fn select(&self, offer: &Message) -> Option<Lease> {
        let xid = idf::system::random();
        let mut request = self.request(REQUEST, xid);
        request.requested = Some(offer.yiaddr);
        request.server = offer.server;
        let mut retry = FIRST_RETRY;
        for _ in 0..4 {
            self.send(&request, broadcast());
            match self.reply(xid, &[ACK, NAK], retry).await {
                Some(ack) if ack.kind == ACK => return Lease::from_ack(&ack),
                Some(_) => return None,
                None => retry = (retry * 2).min(MAX_RETRY),
            }
        }
        None
    }

    /// Renews toward the server, then rebinds toward anyone, until the lease
    /// runs out. `None` on a NAK or at the end of the lease.
    async fn extend(&self, lease: &Lease, bound_at: Instant) -> Option<Lease> {
        loop {
            let elapsed = bound_at.elapsed();
            if elapsed >= lease.lease {
                return None;
            }
            let rebinding = elapsed >= lease.t2;
            let until = if rebinding { lease.lease } else { lease.t2 } - elapsed;
            let wait = (until / 2).max(MIN_RENEW_RETRY).min(until);
            let xid = idf::system::random();
            let mut request = self.request(REQUEST, xid);
            request.ciaddr = lease.config.address;
            request.flags = 0;
            let to = if rebinding { broadcast() } else { SocketAddr::V4(SocketAddrV4::new(lease.server, SERVER_PORT)) };
            self.send(&request, to);
            match self.reply(xid, &[ACK, NAK], wait).await {
                Some(ack) if ack.kind == ACK => return Lease::from_ack(&ack),
                Some(_) => return None,
                None => {}
            }
        }
    }

    fn apply(&self, lease: Option<&Lease>) {
        let mut inner = self.net.lock();
        let config = lease.map(|l| l.config);
        if inner.stack.ipv4(self.iface) != config {
            inner.stack.set_ipv4(self.iface, config);
        }
        inner.stack.set_dns_servers(self.iface, lease.map_or(&[][..], |l| &l.dns));
    }
}

/// Keeps a DHCP lease on `iface` for as long as it runs; drop it to stop.
pub(crate) async fn run(net: &'static Net, iface: IfaceId) {
    let mac = net.lock().stack.link_info(iface).mac;
    let any = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, CLIENT_PORT));
    let Ok(socket) = UdpBuilder::default().interface(iface).rx_buffer(2048).bind_in(net, any) else {
        idf::log::error!(c"devkit_net", "dhcp: port {CLIENT_PORT} on {} is taken", net.lock().stack.name(iface));
        return;
    };
    let client = Client { net, iface, mac, socket };
    loop {
        let mut lease = client.acquire().await;
        loop {
            let bound_at = Instant::now();
            client.apply(Some(&lease));
            sleep(lease.t1).await;
            match client.extend(&lease, bound_at).await {
                Some(next) => lease = next,
                None => break,
            }
        }
        client.apply(None);
    }
}
