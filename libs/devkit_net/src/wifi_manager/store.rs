/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Saved networks in NVS, one slot each, sealed with a key derived from the
 * chip's factory MAC. It only keeps passwords out of a plain flash dump.
 */

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::CStr;

use idf::nvs::{Key, Nvs};

use crate::crypto::{chacha20, siphash};

pub(crate) const SLOTS: usize = 5;
const NAMESPACE: &CStr = c"wifi_manager";
const KEYS: [Key<Vec<u8>>; SLOTS] =
    [Key::new(c"ap0"), Key::new(c"ap1"), Key::new(c"ap2"), Key::new(c"ap3"), Key::new(c"ap4")];
const KEY_ENABLED: Key<bool> = Key::new(c"enabled");
const VERSION: u8 = 1;
const NONCE: usize = 12;
const TAG: usize = 8;
const DOMAIN: (u64, u64) = (u64::from_le_bytes(*b"devkit_n"), u64::from_le_bytes(*b"wifi_mgr"));
const HIDDEN: u8 = 0x01;
const PSK: u8 = 0x02;

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Saved {
    pub ssid: Vec<u8>,
    /// Empty for an open network.
    pub password: String,
    pub hidden: bool,
    /// Higher joined more recently.
    pub seq: u32,
}

pub(crate) struct Cipher {
    key: [u8; 32],
    tag_key: (u64, u64),
}

impl Cipher {
    pub fn new(mac: [u8; 6]) -> Self {
        let mut words = [0u64; 6];
        for (i, word) in words.iter_mut().enumerate() {
            let mut input = [0u8; 7];
            input[..6].copy_from_slice(&mac);
            input[6] = i as u8;
            *word = siphash(DOMAIN.0, DOMAIN.1, &input);
        }
        let mut key = [0u8; 32];
        for (chunk, word) in key.chunks_exact_mut(8).zip(&words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        Self { key, tag_key: (words[4], words[5]) }
    }

    pub fn seal(&self, plain: &[u8], nonce: [u8; NONCE]) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + NONCE + plain.len() + TAG);
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(plain);
        chacha20(&self.key, &nonce, 1, &mut out[1 + NONCE..]);
        let tag = siphash(self.tag_key.0, self.tag_key.1, &out);
        out.extend_from_slice(&tag.to_le_bytes());
        out
    }

    /// `None` when sealed by another chip, by another version, or damaged.
    pub fn open(&self, blob: &[u8]) -> Option<Vec<u8>> {
        if blob.len() < 1 + NONCE + TAG || blob[0] != VERSION {
            return None;
        }
        let (body, tag) = blob.split_at(blob.len() - TAG);
        if siphash(self.tag_key.0, self.tag_key.1, body).to_le_bytes() != tag {
            return None;
        }
        let nonce: [u8; NONCE] = body[1..1 + NONCE].try_into().unwrap();
        let mut plain = body[1 + NONCE..].to_vec();
        chacha20(&self.key, &nonce, 1, &mut plain);
        Some(plain)
    }
}

impl Saved {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.ssid.len() + self.password.len());
        out.push(self.ssid.len() as u8);
        out.extend_from_slice(&self.ssid);
        let mut flags = 0;
        if self.hidden {
            flags |= HIDDEN;
        }
        if !self.password.is_empty() {
            flags |= PSK;
        }
        out.push(flags);
        out.push(self.password.len() as u8);
        out.extend_from_slice(self.password.as_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let (&ssid_len, rest) = data.split_first()?;
        let (ssid, rest) = rest.split_at_checked(usize::from(ssid_len))?;
        let (&flags, rest) = rest.split_first()?;
        let (&password_len, rest) = rest.split_first()?;
        let (password, rest) = rest.split_at_checked(usize::from(password_len))?;
        let seq = u32::from_le_bytes(rest.try_into().ok()?);
        let password = String::from_utf8(password.to_vec()).ok()?;
        if (flags & PSK != 0) == password.is_empty() {
            return None;
        }
        Some(Self { ssid: ssid.to_vec(), password, hidden: flags & HIDDEN != 0, seq })
    }
}

fn nonce() -> [u8; NONCE] {
    let mut nonce = [0u8; NONCE];
    for chunk in nonce.chunks_exact_mut(4) {
        chunk.copy_from_slice(&idf::system::random().to_le_bytes());
    }
    nonce
}

pub(crate) struct Loaded {
    pub slots: [Option<Saved>; SLOTS],
    /// `None` when never set.
    pub enabled: Option<bool>,
}

pub(crate) fn load(cipher: &Cipher) -> idf::Result<Loaded> {
    let nvs = Nvs::open(NAMESPACE)?;
    let mut slots: [Option<Saved>; SLOTS] = Default::default();
    for (slot, key) in slots.iter_mut().zip(KEYS) {
        let Some(blob) = nvs.get(key)? else { continue };
        *slot = cipher.open(&blob).as_deref().and_then(Saved::decode);
        if slot.is_none() {
            idf::log::warn!(super::TAG, "{:?} unreadable, left empty", key.name());
        }
    }
    Ok(Loaded { slots, enabled: nvs.get(KEY_ENABLED)? })
}

pub(crate) struct Write {
    slot: usize,
    blob: Option<Vec<u8>>,
}

impl Write {
    pub fn put(cipher: &Cipher, slot: usize, saved: &Saved) -> Self {
        Self { slot, blob: Some(cipher.seal(&saved.encode(), nonce())) }
    }

    pub fn erase(slot: usize) -> Self {
        Self { slot, blob: None }
    }

    pub fn apply(self) -> idf::Result<()> {
        let mut nvs = Nvs::open(NAMESPACE)?;
        match &self.blob {
            Some(blob) => nvs.set(KEYS[self.slot], blob)?,
            None => nvs.remove(KEYS[self.slot])?,
        }
        nvs.commit()
    }
}

pub(crate) fn save_enabled(enabled: bool) -> idf::Result<()> {
    let mut nvs = Nvs::open(NAMESPACE)?;
    nvs.set(KEY_ENABLED, enabled)?;
    nvs.commit()
}
