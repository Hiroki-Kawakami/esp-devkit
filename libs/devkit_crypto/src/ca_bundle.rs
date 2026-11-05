/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::ed25519::VerifyingKey;
use crate::x509::TrustStore;
use crate::x509::trust::{HEADER, parse_bundle};
use crate::{Error, Result};

mod generated {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/ca_bundle.rs"));
}

const SIGNATURE: usize = 64;

/// The bundle at the start of `data`, its version, and what follows it.
fn split(data: &[u8]) -> Result<(u32, &[u8], &[u8])> {
    let (version, _, entries) = parse_bundle(data)?;
    let len = HEADER + entries.len();
    Ok((version, &data[..len], &data[len..]))
}

/// The bundle Kconfig chose. `Unsupported` when it chose none; an empty store,
/// after an error log, when the partition holds no intact bundle.
pub fn default_bundle() -> Result<TrustStore> {
    #[cfg(CONFIG_DEVKIT_CRYPTO_CA_BUNDLE_EMBEDDED)]
    return TrustStore::from_bundle(generated::BUNDLE);
    #[cfg(CONFIG_DEVKIT_CRYPTO_CA_BUNDLE_PARTITION)]
    return partition::current();
    #[allow(unreachable_code)]
    Err(Error::Unsupported)
}

/// The version of the bundle `default_bundle` returns.
pub fn version() -> Option<u32> {
    #[cfg(CONFIG_DEVKIT_CRYPTO_CA_BUNDLE_EMBEDDED)]
    return parse_bundle(generated::BUNDLE).ok().map(|(v, _, _)| v);
    #[cfg(CONFIG_DEVKIT_CRYPTO_CA_BUNDLE_PARTITION)]
    return partition::version();
    #[allow(unreachable_code)]
    None
}

/// Replaces the partition's bundle with `data`, a bundle followed by its
/// Ed25519 signature under `key`, when it is newer; returns its version.
/// Blocks for the flash erase and write. `Busy` while a store from the bundle
/// before the current one is alive; `Unsupported` unless Kconfig chose the
/// partition.
pub fn install(data: &[u8], key: &VerifyingKey) -> Result<u32> {
    let (version, bundle, signature) = split(data)?;
    if signature.len() != SIGNATURE {
        return Err(Error::Verify);
    }
    key.verify(bundle, signature)?;
    write(bundle, version, true)
}

/// As `install`, without checking a signature or the version.
pub fn install_unsigned(data: &[u8]) -> Result<u32> {
    let (version, bundle, _) = split(data)?;
    write(bundle, version, false)
}

fn write(bundle: &[u8], version: u32, newer_only: bool) -> Result<u32> {
    #[cfg(CONFIG_DEVKIT_CRYPTO_CA_BUNDLE_PARTITION)]
    return partition::write(bundle, version, newer_only);
    #[allow(unreachable_code)]
    {
        let _ = (bundle, version, newer_only);
        Err(Error::Unsupported)
    }
}

#[cfg(CONFIG_DEVKIT_CRYPTO_CA_BUNDLE_PARTITION)]
mod partition {
    use alloc::ffi::CString;
    use alloc::sync::{Arc, Weak};

    use idf::log::error;
    use idf::partition::{Mapping, Partition};
    use idf::sync::Mutex;

    use super::generated;
    use crate::hash::{Hash, Sha256};
    use crate::x509::TrustStore;
    use crate::x509::trust::parse_bundle;
    use crate::{Error, Result};

    const TAG: &core::ffi::CStr = c"devkit_crypto";
    const SECTOR: usize = 4096;
    const MAGIC: &[u8; 4] = b"DKCS";
    const DESCRIPTOR: usize = 44;
    #[cfg(not(target_os = "none"))]
    const SIM_SIZE: usize = 192 * 1024;

    #[derive(Clone, Copy)]
    struct Slot {
        seq: u32,
        len: usize,
        version: u32,
    }

    struct State {
        part: Partition,
        slots: [Option<Slot>; 2],
        maps: [Weak<Mapping>; 2],
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);

    fn open() -> Result<Partition> {
        let label = CString::new(sdkconfig::CONFIG_DEVKIT_CRYPTO_CA_PARTITION).map_err(|_| Error::Unsupported)?;
        #[cfg(not(target_os = "none"))]
        {
            let path = CString::new(alloc::format!("{}.partition", sdkconfig::CONFIG_DEVKIT_CRYPTO_CA_PARTITION)).unwrap();
            let image = CString::new(generated::IMAGE).unwrap();
            Partition::sim_add(&label, SIM_SIZE, &path, Some(&image)).map_err(|_| Error::NoMemory)?;
        }
        let _ = generated::IMAGE;
        Partition::find(&label).ok_or_else(|| {
            error!(TAG, "no partition '{}'", sdkconfig::CONFIG_DEVKIT_CRYPTO_CA_PARTITION);
            Error::Unsupported
        })
    }

    fn slot_size(part: &Partition) -> usize {
        (part.size().saturating_sub(2 * SECTOR) / 2) & !(SECTOR - 1)
    }

    fn slot_offset(part: &Partition, i: usize) -> usize {
        2 * SECTOR + i * slot_size(part)
    }

    fn read_slot(part: &Partition, i: usize) -> Option<Slot> {
        let mut d = [0u8; DESCRIPTOR];
        part.read(i * SECTOR, &mut d).ok()?;
        if &d[..4] != MAGIC {
            return None;
        }
        let seq = u32::from_le_bytes(d[4..8].try_into().unwrap());
        let len = u32::from_le_bytes(d[8..12].try_into().unwrap()) as usize;
        if len > slot_size(part) {
            return None;
        }
        let map = part.map(slot_offset(part, i), len).ok()?;
        if Sha256::digest(map.as_slice()) != d[12..] {
            return None;
        }
        let (version, _, _) = parse_bundle(map.as_slice()).ok()?;
        Some(Slot { seq, len, version })
    }

    fn state(guard: &mut Option<State>) -> Result<&mut State> {
        if guard.is_none() {
            let part = open()?;
            let slots = [read_slot(&part, 0), read_slot(&part, 1)];
            *guard = Some(State { part, slots, maps: [Weak::new(), Weak::new()] });
        }
        Ok(guard.as_mut().unwrap())
    }

    fn current_index(state: &State) -> Option<usize> {
        match state.slots {
            [Some(a), Some(b)] => Some(if b.seq > a.seq { 1 } else { 0 }),
            [Some(_), None] => Some(0),
            [None, Some(_)] => Some(1),
            [None, None] => None,
        }
    }

    pub(super) fn current() -> Result<TrustStore> {
        let mut guard = STATE.lock();
        let state = state(&mut guard)?;
        let Some(i) = current_index(state) else {
            error!(TAG, "no intact CA bundle in the partition");
            return Ok(TrustStore::new());
        };
        let map = match state.maps[i].upgrade() {
            Some(map) => map,
            None => {
                let slot = state.slots[i].unwrap();
                let map = state.part.map(slot_offset(&state.part, i), slot.len).map_err(|e| {
                    error!(TAG, "mapping the CA bundle failed: {}", e.name());
                    Error::NoMemory
                })?;
                let map = Arc::new(map);
                state.maps[i] = Arc::downgrade(&map);
                map
            }
        };
        // The store holds the Arc, which keeps the mapping alive while the slice is in use.
        let data: &'static [u8] = unsafe { core::slice::from_raw_parts(map.as_slice().as_ptr(), map.as_slice().len()) };
        TrustStore::from_mapped(data, Some(map))
    }

    pub(super) fn version() -> Option<u32> {
        let mut guard = STATE.lock();
        let state = state(&mut guard).ok()?;
        current_index(state).map(|i| state.slots[i].unwrap().version)
    }

    pub(super) fn write(bundle: &[u8], version: u32, newer_only: bool) -> Result<u32> {
        let mut guard = STATE.lock();
        let state = state(&mut guard)?;
        let current = current_index(state);
        if newer_only && current.is_some_and(|i| state.slots[i].unwrap().version >= version) {
            return Err(Error::Verify);
        }
        let part = state.part;
        if bundle.len() > slot_size(&part) {
            return Err(Error::InvalidLength);
        }
        let target = current.map_or(0, |i| 1 - i);
        if state.maps[target].strong_count() > 0 {
            return Err(Error::Busy);
        }
        let flash = |e: idf::Error| {
            error!(TAG, "CA bundle flash write failed: {}", e.name());
            Error::Flash(e.code())
        };
        state.slots[target] = None;
        part.erase(target * SECTOR, SECTOR).map_err(flash)?;
        let offset = slot_offset(&part, target);
        part.erase(offset, bundle.len().div_ceil(SECTOR) * SECTOR).map_err(flash)?;
        part.write(offset, bundle).map_err(flash)?;
        let mut written = Sha256::new();
        let mut chunk = [0u8; 256];
        for at in (0..bundle.len()).step_by(chunk.len()) {
            let n = chunk.len().min(bundle.len() - at);
            part.read(offset + at, &mut chunk[..n]).map_err(flash)?;
            written.update(&chunk[..n]);
        }
        let hash = Sha256::digest(bundle);
        if written.finalize() != hash {
            error!(TAG, "CA bundle read back differs from what was written");
            return Err(Error::Verify);
        }
        let seq = current.map_or(0, |i| state.slots[i].unwrap().seq) + 1;
        let mut d = [0u8; DESCRIPTOR];
        d[..4].copy_from_slice(MAGIC);
        d[4..8].copy_from_slice(&seq.to_le_bytes());
        d[8..12].copy_from_slice(&(bundle.len() as u32).to_le_bytes());
        d[12..].copy_from_slice(&hash);
        part.write(target * SECTOR, &d).map_err(flash)?;
        state.slots[target] = Some(Slot { seq, len: bundle.len(), version });
        Ok(version)
    }
}
