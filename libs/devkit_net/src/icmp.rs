/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::net::SocketAddrV4;
use core::time::Duration;

use crate::Error;
use crate::stack::{Stack, Tx};
use crate::time::Instant;
use crate::wire::{
    ICMP_ECHO_REPLY, ICMP_ECHO_REQUEST, IPV4_HEADER, Ipv4, PROTO_ICMP, PROTO_TCP, PROTO_UDP, be16, checksum, put16, sum,
};

const UNREACHABLE: u8 = 3;
const TIME_EXCEEDED: u8 = 11;
const PARAMETER_PROBLEM: u8 = 12;
pub(crate) const CODE_PROTOCOL: u8 = 2;
pub(crate) const CODE_PORT: u8 = 3;

const ERRORS_PER_SECOND: u32 = 10;
const QUOTED: usize = 8;

/// What an ICMP error says about a connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reported {
    /// Port or protocol unreachable: nobody is there.
    Refused,
    /// Anything else; the peer may still answer later.
    Unreachable,
}

impl From<Reported> for Error {
    fn from(r: Reported) -> Self {
        match r {
            Reported::Refused => Error::ConnectionRefused,
            Reported::Unreachable => Error::HostUnreachable,
        }
    }
}

/// Paces the errors the stack sends.
pub(crate) struct Limiter {
    tokens: u32,
    refilled: Instant,
}

impl Default for Limiter {
    fn default() -> Self {
        Self { tokens: ERRORS_PER_SECOND, refilled: Instant::ZERO }
    }
}

impl Limiter {
    fn take(&mut self, now: Instant) -> bool {
        if now - self.refilled >= Duration::from_secs(1) {
            self.tokens = ERRORS_PER_SECOND;
            self.refilled = now;
        }
        let ok = self.tokens > 0;
        self.tokens = self.tokens.saturating_sub(1);
        ok
    }
}

impl Stack {
    pub(crate) fn icmp_input(&mut self, ip: &Ipv4, unicast: bool, p: &[u8], now: Instant, tx: &mut Tx) {
        if p.len() < 8 || checksum(sum(p, 0)) != 0 {
            return;
        }
        match p[0] {
            ICMP_ECHO_REQUEST if p[1] == 0 && unicast => {
                let _ = self.send_ipv4(Some(ip.dst), ip.src, PROTO_ICMP, p.len(), now, tx, |out, _| {
                    out.copy_from_slice(p);
                    out[0] = ICMP_ECHO_REPLY;
                    put16(out, 2, 0);
                    let c = checksum(sum(out, 0));
                    put16(out, 2, c);
                });
            }
            UNREACHABLE | TIME_EXCEEDED | PARAMETER_PROBLEM => self.icmp_error_input(p, now, tx),
            _ => {}
        }
    }

    fn icmp_error_input(&mut self, p: &[u8], now: Instant, tx: &mut Tx) {
        let Some((quoted, rest)) = Ipv4::parse_quoted(&p[8..]) else { return };
        if rest.len() < QUOTED {
            return;
        }
        let reported = match (p[0], p[1]) {
            (UNREACHABLE, CODE_PROTOCOL | CODE_PORT) => Reported::Refused,
            _ => Reported::Unreachable,
        };
        let local = SocketAddrV4::new(quoted.src, be16(rest, 0));
        let remote = SocketAddrV4::new(quoted.dst, be16(rest, 2));
        match quoted.protocol {
            PROTO_TCP => {
                let seq = u32::from_be_bytes(rest[4..8].try_into().unwrap());
                self.tcp_icmp_error(local, remote, seq, reported, now, tx);
            }
            PROTO_UDP => self.udp_icmp_error(local, remote, reported),
            _ => {}
        }
    }

    /// Tells the sender of `ip` that nothing took it. Never about a broadcast,
    /// an unspecified sender or another ICMP error, and at most ten a second.
    pub(crate) fn send_unreachable(&mut self, ip: &Ipv4, payload: &[u8], code: u8, now: Instant, tx: &mut Tx) {
        if ip.src.is_unspecified() || !self.icmp_limit.take(now) {
            return;
        }
        let quoted = payload.len().min(QUOTED);
        let original = Ipv4 { frag: 0, ..*ip };
        let len = 8 + IPV4_HEADER + quoted;
        let _ = self.send_ipv4(Some(ip.dst), ip.src, PROTO_ICMP, len, now, tx, |out, _| {
            out[..8].fill(0);
            out[0] = UNREACHABLE;
            out[1] = code;
            original.write_header(&mut out[8..8 + IPV4_HEADER], payload.len());
            out[8 + IPV4_HEADER..].copy_from_slice(&payload[..quoted]);
            let c = checksum(sum(out, 0));
            put16(out, 2, c);
        });
    }
}
