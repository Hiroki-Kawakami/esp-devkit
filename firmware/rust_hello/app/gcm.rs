/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * `gcm`: AES-GCM in internal RAM, where the accelerator takes it, against
 * the same in PSRAM, where PSA does.
 */

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use devkit_crypto::aead::{Aead, Algorithm, NONCE, TAG};
use idf::heap::{self, Caps};
use idf::time::Instant;

const MAX: usize = 17 * 1024;

fn buffer(caps: Caps) -> Result<Vec<u8, Caps>, String> {
    let mut v = heap::try_vec(MAX + TAG + 128, caps).map_err(|e| format!("alloc: {e}"))?;
    v.resize(v.capacity(), 0);
    Ok(v)
}

fn seal(aead: &Aead, nonce: &[u8; NONCE], aad: &[u8], mem: &mut [u8], at: usize, plain: &[u8]) -> Result<Vec<u8>, String> {
    let buf = &mut mem[at..at + plain.len() + TAG];
    buf[..plain.len()].copy_from_slice(plain);
    aead.seal_in_place(nonce, aad, buf, plain.len()).map_err(|e| format!("seal {}@{at}: {e:?}", plain.len()))?;
    Ok(buf.to_vec())
}

fn open(aead: &Aead, nonce: &[u8; NONCE], aad: &[u8], mem: &mut [u8], at: usize, sealed: &[u8]) -> Result<Vec<u8>, String> {
    let buf = &mut mem[at..at + sealed.len()];
    buf.copy_from_slice(sealed);
    aead.open_in_place(nonce, aad, buf).map(|p| p.to_vec()).map_err(|e| format!("open {}@{at}: {e:?}", sealed.len()))
}

fn check(aead: &Aead, internal: &mut [u8], psram: &mut [u8], len: usize, at: usize, aad_len: usize) -> Result<(), String> {
    let mut plain = alloc::vec![0u8; len];
    devkit_crypto::rand::fill(&mut plain);
    let mut nonce = [0; NONCE];
    devkit_crypto::rand::fill(&mut nonce);
    let mut aad = alloc::vec![0u8; aad_len];
    devkit_crypto::rand::fill(&mut aad);
    let base = internal.as_ptr().align_offset(64);
    let hw = seal(aead, &nonce, &aad, internal, base + at, &plain)?;
    let sw = seal(aead, &nonce, &aad, psram, at, &plain)?;
    if hw != sw {
        return Err(format!("sealed {len}@{at} aad {aad_len}: differs"));
    }
    if open(aead, &nonce, &aad, internal, base + at, &sw)? != plain || open(aead, &nonce, &aad, psram, at, &hw)? != plain {
        return Err(format!("opened {len}@{at} aad {aad_len}: differs"));
    }
    let mut bad = hw.clone();
    let flip = (len + at) % bad.len();
    bad[flip] ^= 1;
    if open(aead, &nonce, &aad, internal, base + at, &bad).is_ok() {
        return Err(format!("tampered {len}@{at} aad {aad_len}: accepted"));
    }
    Ok(())
}

fn time(aead: &Aead, mem: &mut [u8], len: usize) -> Result<(u32, u32), String> {
    let nonce = [3; NONCE];
    let at = mem.as_ptr().align_offset(64) + 5;
    let plain = alloc::vec![7u8; len];
    let sealed = seal(aead, &nonce, &[0; 5], mem, at, &plain)?;
    let (mut s, mut o) = (0, 0);
    const N: u32 = 20;
    for _ in 0..N {
        let t = Instant::now();
        seal(aead, &nonce, &[0; 5], mem, at, &plain)?;
        s += t.elapsed().as_micros() as u32;
        let t = Instant::now();
        open(aead, &nonce, &[0; 5], mem, at, &sealed)?;
        o += t.elapsed().as_micros() as u32;
    }
    Ok((s / N, o / N))
}

/// Runs on a thread of its own and logs the outcome: it outlasts a reply.
pub fn command(_: &[&str]) -> Result<Option<String>, String> {
    idf::thread::Builder::new()
        .name("gcm")
        .stack_size(16 * 1024)
        .spawn(|| match run() {
            Ok(out) => idf::log::info!(c"gcm", "{out}"),
            Err(err) => idf::log::error!(c"gcm", "{err}"),
        })
        .map(|_| None)
        .map_err(|_| String::from("spawn"))
}

fn run() -> Result<String, String> {
    let mut internal = buffer(Caps::INTERNAL | Caps::DMA)?;
    let mut psram = buffer(Caps::SPIRAM)?;
    let mut checked = 0;
    for (algorithm, key_len) in [(Algorithm::Aes128Gcm, 16), (Algorithm::Aes256Gcm, 32)] {
        let mut key = alloc::vec![0u8; key_len];
        devkit_crypto::rand::fill(&mut key);
        let aead = Aead::new(algorithm, &key).map_err(|e| format!("key: {e:?}"))?;
        for len in [1, 15, 16, 17, 63, 64, 65, 127, 128, 1000, 4031, 4032, 4033, 8064, 12345, 16384 + 17, MAX] {
            for at in [0, 5, 16, 59, 63] {
                for aad_len in [0, 5, 13, 64, 65] {
                    check(&aead, &mut internal, &mut psram, len, at, aad_len)?;
                    checked += 1;
                }
            }
        }
    }
    let aead = Aead::new(Algorithm::Aes128Gcm, &[9; 16]).map_err(|e| format!("key: {e:?}"))?;
    let mut out = format!("{checked} ok;");
    for len in [1024, 4096, 16384] {
        let (hs, ho) = time(&aead, &mut internal, len)?;
        let (ss, so) = time(&aead, &mut psram, len)?;
        out += &format!(" {len}: hw {hs}/{ho}us psa {ss}/{so}us;");
    }
    Ok(out)
}
