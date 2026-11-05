/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use super::Certificate;
use crate::{Error, Result};

pub(crate) const MAGIC: &[u8; 4] = b"DKCA";
pub(crate) const FORMAT: u8 = 1;
pub(crate) const HEADER: usize = 20;

/// A trusted subject and its key; a root needs nothing else to anchor a chain.
#[derive(Clone, Copy, Debug)]
pub struct Anchor<'a> {
    pub subject: &'a [u8],
    pub spki: &'a [u8],
    /// Certificates issued under this root after this time (Unix seconds) are not trusted.
    pub distrust_after: Option<i64>,
}

pub(crate) struct Bundle {
    pub version: u32,
    pub count: u32,
    pub entries: &'static [u8],
}

/// The bundle's header and entries; `data` may carry a signature after them.
pub(crate) fn parse_bundle(data: &[u8]) -> Result<(u32, u32, &[u8])> {
    if data.len() < HEADER || &data[..4] != MAGIC || data[4] != FORMAT {
        return Err(Error::Decode);
    }
    let word = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
    let (version, count, len) = (word(8), word(12), word(16) as usize);
    let entries = data.get(HEADER..HEADER + len).ok_or(Error::Decode)?;
    let mut rest = entries;
    for _ in 0..count {
        let (_, tail) = next_entry(rest)?;
        rest = tail;
    }
    if !rest.is_empty() {
        return Err(Error::Decode);
    }
    Ok((version, count, entries))
}

fn next_entry(data: &[u8]) -> Result<(Anchor<'_>, &[u8])> {
    if data.len() < 12 {
        return Err(Error::Decode);
    }
    let subject_len = u16::from_le_bytes([data[0], data[1]]) as usize;
    let spki_len = u16::from_le_bytes([data[2], data[3]]) as usize;
    let distrust = i64::from_le_bytes(data[4..12].try_into().unwrap());
    let body = &data[12..];
    if body.len() < subject_len + spki_len {
        return Err(Error::Decode);
    }
    let anchor = Anchor {
        subject: &body[..subject_len],
        spki: &body[subject_len..subject_len + spki_len],
        distrust_after: (distrust != 0).then_some(distrust),
    };
    Ok((anchor, &body[subject_len + spki_len..]))
}

/// The roots a chain may end in: a CA bundle, the anchors added to it, or both.
#[derive(Default, Clone)]
pub struct TrustStore {
    bundle: Option<Arc<Bundle>>,
    // Keeps a mapped bundle mapped while any store uses it.
    _mapping: Option<Arc<dyn Any + Send + Sync>>,
    extra: Vec<(Vec<u8>, Vec<u8>)>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mozilla's roots as Kconfig placed them; see `ca_bundle::default_bundle`.
    pub fn default_bundle() -> Result<Self> {
        crate::ca_bundle::default_bundle()
    }

    /// From a bundle in this crate's format, without a signature check.
    pub fn from_bundle(data: &'static [u8]) -> Result<Self> {
        Self::from_mapped(data, None)
    }

    pub(crate) fn from_mapped(data: &'static [u8], mapping: Option<Arc<dyn Any + Send + Sync>>) -> Result<Self> {
        let (version, count, entries) = parse_bundle(data)?;
        Ok(Self { bundle: Some(Arc::new(Bundle { version, count, entries })), _mapping: mapping, extra: Vec::new() })
    }

    /// Trusts the subject and key of `der`, whatever its issuer.
    pub fn add_der(&mut self, der: &[u8]) -> Result<()> {
        let cert = Certificate::parse(der)?;
        self.extra.push((cert.subject.to_vec(), cert.spki.to_vec()));
        Ok(())
    }

    pub fn add_anchor(&mut self, subject: &[u8], spki: &[u8]) {
        self.extra.push((subject.to_vec(), spki.to_vec()));
    }

    /// The bundle's version, when there is one.
    pub fn bundle_version(&self) -> Option<u32> {
        self.bundle.as_ref().map(|b| b.version)
    }

    pub fn len(&self) -> usize {
        self.bundle.as_ref().map_or(0, |b| b.count as usize) + self.extra.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn anchors(&self) -> impl Iterator<Item = Anchor<'_>> {
        let mut rest = self.bundle.as_ref().map_or(&[][..], |b| b.entries);
        let bundled = core::iter::from_fn(move || {
            let (anchor, tail) = next_entry(rest).ok()?;
            rest = tail;
            Some(anchor)
        });
        let extra = self.extra.iter().map(|(subject, spki)| Anchor { subject, spki, distrust_after: None });
        bundled.chain(extra)
    }
}
