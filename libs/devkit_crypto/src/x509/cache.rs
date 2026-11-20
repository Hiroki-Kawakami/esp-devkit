/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Intermediate certificates whose signature already checked out, by the
//! certificate and the signer's key, so a chain seen again skips the
//! signature. RAM only, most recently used kept.

use alloc::vec::Vec;

use idf::sync::Mutex;

use super::{Certificate, PublicKey};
use crate::Result;
use crate::hash::{Hash, Sha256};

const ENTRIES: usize = 16;

type Entry = ([u8; 32], [u8; 32]);

static CACHE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// `cert` signed by `key`, whose SubjectPublicKeyInfo is `spki`.
pub(super) fn signed_by(cert: &Certificate, spki: &[u8], key: &PublicKey) -> Result<()> {
    let entry = (Sha256::digest(cert.der), Sha256::digest(spki));
    {
        let mut cache = CACHE.lock();
        if let Some(i) = cache.iter().position(|e| *e == entry) {
            let hit = cache.remove(i);
            cache.push(hit);
            return Ok(());
        }
    }
    cert.verify_signed_by(key)?;
    let mut cache = CACHE.lock();
    if cache.contains(&entry) {
        return Ok(());
    }
    if cache.len() == ENTRIES {
        cache.remove(0);
    }
    cache.push(entry);
    Ok(())
}

#[cfg(test)]
pub(crate) fn contains(cert: &[u8], spki: &[u8]) -> bool {
    CACHE.lock().contains(&(Sha256::digest(cert), Sha256::digest(spki)))
}
