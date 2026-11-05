/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::net::IpAddr;

use super::{Certificate, KEY_USAGE_DIGITAL_SIGNATURE, KEY_USAGE_KEY_CERT_SIGN, PublicKey, TrustStore, oid};
use crate::der::{self, Reader};
use crate::{CertError, Error, Result};

const MAX_DEPTH: usize = 8;

/// The name a server certificate must carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServerName<'a> {
    Dns(&'a str),
    Ip(IpAddr),
}

impl<'a> ServerName<'a> {
    /// An IP address if `name` parses as one, a DNS name otherwise.
    pub fn parse(name: &'a str) -> Self {
        match name.parse() {
            Ok(ip) => ServerName::Ip(ip),
            Err(_) => ServerName::Dns(name),
        }
    }
}

fn fail(e: CertError) -> Error {
    Error::Certificate(e)
}

/// Checks a TLS server chain, leaf first, against `store`. `now` in Unix
/// seconds; `None` skips validity periods, for a device that has no clock yet.
pub fn verify_chain(chain: &[&[u8]], store: &TrustStore, now: Option<i64>, name: Option<&ServerName>) -> Result<()> {
    let leaf = Certificate::parse(chain.first().ok_or(fail(CertError::UnknownIssuer))?)?;
    check_time(&leaf, now)?;
    if let Some(name) = name {
        check_name(&leaf, name)?;
    }
    if !leaf.has_ext_key_usage(oid::SERVER_AUTH) {
        return Err(fail(CertError::BadUsage));
    }
    if leaf.key_usage.is_some_and(|u| u & KEY_USAGE_DIGITAL_SIGNATURE == 0) {
        return Err(fail(CertError::BadUsage));
    }
    if store.anchors().any(|a| a.spki == leaf.spki) {
        return Ok(());
    }
    let intermediates: Vec<Certificate> = chain[1..].iter().filter_map(|der| Certificate::parse(der).ok()).collect();
    let mut path = Vec::new();
    let mut best = fail(CertError::UnknownIssuer);
    if build(&leaf, &leaf, &intermediates, store, now, &mut path, &mut best) { Ok(()) } else { Err(best) }
}

fn build<'a>(
    leaf: &Certificate,
    cert: &Certificate,
    intermediates: &'a [Certificate<'a>],
    store: &TrustStore,
    now: Option<i64>,
    path: &mut Vec<usize>,
    best: &mut Error,
) -> bool {
    for anchor in store.anchors().filter(|a| a.subject == cert.issuer) {
        let Ok(key) = PublicKey::from_spki(anchor.spki) else { continue };
        if let Err(e) = cert.verify_signed_by(&key) {
            *best = e;
            continue;
        }
        if anchor.distrust_after.is_some_and(|t| leaf.not_before > t) {
            *best = fail(CertError::Distrusted);
            continue;
        }
        return true;
    }
    if path.len() >= MAX_DEPTH {
        *best = fail(CertError::PathTooLong);
        return false;
    }
    for (i, issuer) in intermediates.iter().enumerate() {
        if path.contains(&i) || issuer.subject != cert.issuer {
            continue;
        }
        if let Err(e) = check_issuer(issuer, path.len(), now).and_then(|_| check_constraints(issuer, leaf)) {
            *best = e;
            continue;
        }
        let Ok(key) = issuer.public_key() else { continue };
        if let Err(e) = cert.verify_signed_by(&key) {
            *best = e;
            continue;
        }
        path.push(i);
        if build(leaf, issuer, intermediates, store, now, path, best) {
            return true;
        }
        path.pop();
    }
    false
}

fn check_time(cert: &Certificate, now: Option<i64>) -> Result<()> {
    match now {
        Some(t) if t < cert.not_before => Err(fail(CertError::NotYetValid)),
        Some(t) if t > cert.not_after => Err(fail(CertError::Expired)),
        _ => Ok(()),
    }
}

fn check_issuer(issuer: &Certificate, below: usize, now: Option<i64>) -> Result<()> {
    check_time(issuer, now)?;
    let Some(bc) = issuer.basic_constraints.filter(|bc| bc.ca) else { return Err(fail(CertError::NotCa)) };
    if bc.path_len.is_some_and(|n| (below as u32) > n) {
        return Err(fail(CertError::PathTooLong));
    }
    if issuer.key_usage.is_some_and(|u| u & KEY_USAGE_KEY_CERT_SIGN == 0) {
        return Err(fail(CertError::BadUsage));
    }
    if !issuer.has_ext_key_usage(oid::SERVER_AUTH) {
        return Err(fail(CertError::BadUsage));
    }
    Ok(())
}

fn check_name(leaf: &Certificate, name: &ServerName) -> Result<()> {
    let found = match name {
        ServerName::Dns(host) => {
            let host = host.strip_suffix('.').unwrap_or(host);
            leaf.dns_names().any(|pattern| dns_matches(pattern, host.as_bytes()))
        }
        ServerName::Ip(ip) => leaf.ip_addresses().any(|addr| addr == ip_bytes(ip).as_slice()),
    };
    if found { Ok(()) } else { Err(fail(CertError::NameMismatch)) }
}

fn ip_bytes(ip: &IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(v4) => v4.octets().to_vec(),
        IpAddr::V6(v6) => v6.octets().to_vec(),
    }
}

/// RFC 6125: a `*` only as the whole leftmost label, matching one label.
fn dns_matches(pattern: &[u8], host: &[u8]) -> bool {
    match pattern.strip_prefix(b"*.") {
        Some(suffix) => match host.iter().position(|&c| c == b'.') {
            Some(dot) if dot > 0 => host[dot + 1..].eq_ignore_ascii_case(suffix) && suffix.contains(&b'.'),
            _ => false,
        },
        None => pattern.eq_ignore_ascii_case(host),
    }
}

fn dns_within(name: &[u8], base: &[u8]) -> bool {
    let base = base.strip_prefix(b".").unwrap_or(base);
    if base.is_empty() {
        return true;
    }
    name.eq_ignore_ascii_case(base)
        || (name.len() > base.len()
            && name[name.len() - base.len()..].eq_ignore_ascii_case(base)
            && name[name.len() - base.len() - 1] == b'.')
}

fn ip_within(addr: &[u8], base: &[u8]) -> bool {
    base.len() == 2 * addr.len() && addr.iter().zip(&base[..addr.len()]).zip(&base[addr.len()..]).all(|((a, b), m)| a & m == b & m)
}

/// The issuer's name constraints over the leaf's DNS names and IP addresses.
fn check_constraints(issuer: &Certificate, leaf: &Certificate) -> Result<()> {
    let Some(value) = issuer.name_constraints else { return Ok(()) };
    let mut seq = Reader::new(value).sequence()?;
    let permitted = seq.read_optional(der::context(0))?;
    let excluded = seq.read_optional(der::context(1))?;
    fn subtrees(list: Option<&[u8]>, tag: u8) -> Result<Vec<&[u8]>> {
        let mut out = Vec::new();
        let mut r = Reader::new(list.unwrap_or(&[]));
        while !r.is_empty() {
            let mut subtree = r.sequence()?;
            let (t, base, _) = subtree.read_any()?;
            if t == tag {
                out.push(base);
            }
        }
        Ok(out)
    }
    let checks: [(u8, fn(&[u8], &[u8]) -> bool, Vec<&[u8]>); 2] = [
        (der::context_primitive(2), dns_within, leaf.dns_names().collect()),
        (der::context_primitive(7), ip_within, leaf.ip_addresses().collect()),
    ];
    for (tag, within, names) in checks {
        let allowed = subtrees(permitted, tag)?;
        let denied = subtrees(excluded, tag)?;
        for name in names {
            if denied.iter().any(|base| within(name, base)) || (!allowed.is_empty() && !allowed.iter().any(|base| within(name, base))) {
                return Err(fail(CertError::NameConstraint));
            }
        }
    }
    Ok(())
}
