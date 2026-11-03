/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::net::Ipv4Addr;

pub(crate) type Mac = [u8; 6];

pub(crate) const BROADCAST_MAC: Mac = [0xff; 6];
pub(crate) const ETHERNET_HEADER: usize = 14;
pub(crate) const ETHERNET_MIN_FRAME: usize = 60;
pub(crate) const ETHERTYPE_IPV4: u16 = 0x0800;
pub(crate) const ETHERTYPE_ARP: u16 = 0x0806;

pub(crate) const IPV4_HEADER: usize = 20;
pub(crate) const PROTO_ICMP: u8 = 1;
pub(crate) const PROTO_UDP: u8 = 17;
pub(crate) const UDP_HEADER: usize = 8;
pub(crate) const DEFAULT_TTL: u8 = 64;

pub(crate) const ICMP_ECHO_REPLY: u8 = 0;
pub(crate) const ICMP_ECHO_REQUEST: u8 = 8;

pub(crate) const ARP_LEN: usize = 28;
pub(crate) const ARP_REQUEST: u16 = 1;
pub(crate) const ARP_REPLY: u16 = 2;

pub(crate) fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

pub(crate) fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_be_bytes());
}

fn ipv4_at(b: &[u8], at: usize) -> Ipv4Addr {
    Ipv4Addr::new(b[at], b[at + 1], b[at + 2], b[at + 3])
}

fn mac_at(b: &[u8], at: usize) -> Mac {
    b[at..at + 6].try_into().unwrap()
}

pub(crate) fn sum(data: &[u8], mut acc: u32) -> u32 {
    let mut pairs = data.chunks_exact(2);
    for p in &mut pairs {
        acc += u32::from(u16::from_be_bytes([p[0], p[1]]));
    }
    if let [last] = pairs.remainder() {
        acc += u32::from(*last) << 8;
    }
    acc
}

/// The one's complement of the folded sum; 0 over data that carries a valid
/// checksum.
pub(crate) fn checksum(acc: u32) -> u16 {
    let mut acc = acc;
    while acc > 0xffff {
        acc = (acc & 0xffff) + (acc >> 16);
    }
    !(acc as u16)
}

pub(crate) struct Ethernet<'a> {
    pub dst: Mac,
    pub src: Mac,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

impl<'a> Ethernet<'a> {
    pub fn parse(frame: &'a [u8]) -> Option<Self> {
        if frame.len() < ETHERNET_HEADER {
            return None;
        }
        Some(Self {
            dst: mac_at(frame, 0),
            src: mac_at(frame, 6),
            ethertype: be16(frame, 12),
            payload: &frame[ETHERNET_HEADER..],
        })
    }

    pub fn write_header(buf: &mut [u8], dst: Mac, src: Mac, ethertype: u16) {
        buf[0..6].copy_from_slice(&dst);
        buf[6..12].copy_from_slice(&src);
        put16(buf, 12, ethertype);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Arp {
    pub op: u16,
    pub sha: Mac,
    pub spa: Ipv4Addr,
    pub tha: Mac,
    pub tpa: Ipv4Addr,
}

impl Arp {
    pub fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < ARP_LEN || be16(p, 0) != 1 || be16(p, 2) != ETHERTYPE_IPV4 || p[4] != 6 || p[5] != 4 {
            return None;
        }
        Some(Self { op: be16(p, 6), sha: mac_at(p, 8), spa: ipv4_at(p, 14), tha: mac_at(p, 18), tpa: ipv4_at(p, 24) })
    }

    pub fn write(&self, p: &mut [u8]) {
        put16(p, 0, 1);
        put16(p, 2, ETHERTYPE_IPV4);
        p[4] = 6;
        p[5] = 4;
        put16(p, 6, self.op);
        p[8..14].copy_from_slice(&self.sha);
        p[14..18].copy_from_slice(&self.spa.octets());
        p[18..24].copy_from_slice(&self.tha);
        p[24..28].copy_from_slice(&self.tpa.octets());
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Ipv4 {
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub protocol: u8,
    pub ttl: u8,
    pub ident: u16,
    pub fragmented: bool,
}

impl Ipv4 {
    /// Checks the header and its checksum and cuts the payload to the total
    /// length, dropping link padding.
    pub fn parse(p: &[u8]) -> Option<(Self, &[u8])> {
        if p.len() < IPV4_HEADER || p[0] >> 4 != 4 {
            return None;
        }
        let header = usize::from(p[0] & 0x0f) * 4;
        let total = usize::from(be16(p, 2));
        if header < IPV4_HEADER || total < header || total > p.len() || checksum(sum(&p[..header], 0)) != 0 {
            return None;
        }
        let flags = be16(p, 6);
        let ip = Self {
            src: ipv4_at(p, 12),
            dst: ipv4_at(p, 16),
            protocol: p[9],
            ttl: p[8],
            ident: be16(p, 4),
            fragmented: flags & 0x2000 != 0 || flags & 0x1fff != 0,
        };
        Some((ip, &p[header..total]))
    }

    pub fn write_header(&self, p: &mut [u8], payload_len: usize) {
        p[0] = 0x45;
        p[1] = 0;
        put16(p, 2, (IPV4_HEADER + payload_len) as u16);
        put16(p, 4, self.ident);
        put16(p, 6, 0);
        p[8] = self.ttl;
        p[9] = self.protocol;
        put16(p, 10, 0);
        p[12..16].copy_from_slice(&self.src.octets());
        p[16..20].copy_from_slice(&self.dst.octets());
        let c = checksum(sum(&p[..IPV4_HEADER], 0));
        put16(p, 10, c);
    }
}
