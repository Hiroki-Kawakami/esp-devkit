/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::net::Ipv4Addr;

use crate::wire::{be16, put16};

pub(crate) const PORT: u16 = 53;
pub(crate) const HEADER: usize = 12;
pub(crate) const TYPE_A: u16 = 1;
pub(crate) const TYPE_CNAME: u16 = 5;
pub(crate) const TYPE_SOA: u16 = 6;
pub(crate) const CLASS_IN: u16 = 1;
pub(crate) const RCODE_NOERROR: u8 = 0;
pub(crate) const RCODE_NXDOMAIN: u8 = 3;
pub(crate) const FLAG_QR: u16 = 0x8000;
pub(crate) const FLAG_TC: u16 = 0x0200;
pub(crate) const FLAG_RD: u16 = 0x0100;
const OPCODE: u16 = 0x7800;
const MAX_NAME: usize = 253;
const MAX_LABEL: usize = 63;

/// Lower case, labels joined by dots, without the root's.
pub(crate) type Name = Vec<u8>;

/// `None` unless `host` is an ASCII name a query can carry.
pub(crate) fn parse_name(host: &str) -> Option<Name> {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.len() > MAX_NAME {
        return None;
    }
    let valid = host.split('.').all(|l| (1..=MAX_LABEL).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_graphic()));
    valid.then(|| host.as_bytes().to_ascii_lowercase())
}

pub(crate) fn put_name(out: &mut Vec<u8>, name: &[u8]) {
    for label in name.split(|&b| b == b'.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label);
    }
    out.push(0);
}

pub(crate) fn query(id: u16, name: &[u8]) -> Vec<u8> {
    let mut out = alloc::vec![0; HEADER];
    put16(&mut out, 0, id);
    put16(&mut out, 2, FLAG_RD);
    put16(&mut out, 4, 1);
    put_name(&mut out, name);
    out.extend_from_slice(&TYPE_A.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    out
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Response {
    pub id: u16,
    pub truncated: bool,
    pub rcode: u8,
    pub question: Name,
    /// Owner, target, TTL.
    pub cnames: Vec<(Name, Name, u32)>,
    pub addrs: Vec<(Name, Ipv4Addr, u32)>,
    /// From an SOA in the authority section (RFC 2308 5).
    pub negative_ttl: Option<u32>,
}

/// A response to an A query; a truncated one only up to its question.
pub(crate) fn decode(msg: &[u8]) -> Option<Response> {
    if msg.len() < HEADER {
        return None;
    }
    let flags = be16(msg, 2);
    if flags & FLAG_QR == 0 || flags & OPCODE != 0 || be16(msg, 4) != 1 {
        return None;
    }
    let (question, at) = read_name(msg, HEADER)?;
    if msg.get(at..at + 4)? != [0, TYPE_A as u8, 0, CLASS_IN as u8] {
        return None;
    }
    let mut response = Response {
        id: be16(msg, 0),
        truncated: flags & FLAG_TC != 0,
        rcode: (flags & 0x000f) as u8,
        question,
        cnames: Vec::new(),
        addrs: Vec::new(),
        negative_ttl: None,
    };
    if response.truncated {
        return Some(response);
    }
    let mut at = at + 4;
    for i in 0..usize::from(be16(msg, 6)) + usize::from(be16(msg, 8)) {
        let answer = i < usize::from(be16(msg, 6));
        let (owner, next) = read_name(msg, at)?;
        let fixed = msg.get(next..next + 10)?;
        let (kind, class, ttl, len) = (be16(fixed, 0), be16(fixed, 2), ttl(fixed, 4), usize::from(be16(fixed, 8)));
        let data = next + 10;
        let rdata = msg.get(data..data + len)?;
        at = data + len;
        if class != CLASS_IN {
            continue;
        }
        match (answer, kind) {
            (true, TYPE_A) if len == 4 => {
                response.addrs.push((owner, Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]), ttl));
            }
            (true, TYPE_CNAME) => response.cnames.push((owner, read_name(msg, data)?.0, ttl)),
            (false, TYPE_SOA) => {
                let (_, after_mname) = read_name(msg, data)?;
                let (_, after_rname) = read_name(msg, after_mname)?;
                let minimum = ttl_at(msg.get(after_rname + 16..after_rname + 20)?);
                response.negative_ttl = Some(ttl.min(minimum));
            }
            _ => {}
        }
    }
    Some(response)
}

/// RFC 2181 8: a TTL with the top bit set counts as zero.
fn ttl(b: &[u8], at: usize) -> u32 {
    ttl_at(&b[at..at + 4])
}

fn ttl_at(b: &[u8]) -> u32 {
    let v = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    if v > i32::MAX as u32 { 0 } else { v }
}

/// The name at `at` and where the record goes on after it.
fn read_name(msg: &[u8], at: usize) -> Option<(Name, usize)> {
    let mut name = Vec::new();
    let mut pos = at;
    let mut end = None;
    loop {
        let len = usize::from(*msg.get(pos)?);
        match len & 0xc0 {
            0 if len == 0 => break,
            0 => {
                let label = msg.get(pos + 1..pos + 1 + len)?;
                if label.contains(&b'.') {
                    return None;
                }
                if !name.is_empty() {
                    name.push(b'.');
                }
                name.extend(label.iter().map(u8::to_ascii_lowercase));
                if name.len() > MAX_NAME {
                    return None;
                }
                pos += 1 + len;
            }
            0xc0 => {
                let target = (len & 0x3f) << 8 | usize::from(*msg.get(pos + 1)?);
                // Only backwards, so that a loop runs into the length limit.
                if target >= pos {
                    return None;
                }
                end.get_or_insert(pos + 2);
                pos = target;
            }
            _ => return None,
        }
    }
    Some((name, end.unwrap_or(pos + 1)))
}
