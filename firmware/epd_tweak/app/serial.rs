/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! The commands of `tool/index.html`, on the harness line protocol; every
//! line stays within its 512 bytes.
//!
//! - `tweak`: the most frames a waveform takes, or `unsupported`.
//! - `wf begin <slot> <steps>`, `wf data <hex>`..., `wf end`, `wf abort`:
//!   replaces a waveform, 8 hex digits a word, 16 words a frame.
//! - `dump <slot>`: `<slot> <steps> <hex>` of what the slot replays.
//! - `refresh <fast|quality|text> [all]`, `clear`, `pattern`.
//! - `img begin <bytes>`, `img data <base64>`..., `img end`, `img abort`:
//!   shows a JPEG or PNG fitted to the panel.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use bsp_rs::display::{Display, Rect, Waveform};
use idf::sync::{Mutex, channel};

use crate::Job;

const IMG_MAX: usize = 4 * 1024 * 1024;

type Reply = Result<Option<String>, String>;

pub(crate) fn register(display: &'static Display, jobs: idf::sync::Sender<Job>) {
    waveforms(display);
    let Some(epd) = display.epd() else { return };
    let full = {
        let size = display.size();
        Rect::new(0, 0, size.width as i16, size.height as i16)
    };
    harness_rs::register("refresh", move |args| {
        let waveform = match args.first() {
            Some(&"fast") => Waveform::Fast,
            Some(&"quality") => Waveform::Quality,
            Some(&"text") => Waveform::Text,
            _ => return Err("usage: refresh <fast|quality|text> [all]".to_string()),
        };
        epd.refresh(full, waveform, args.get(1) == Some(&"all")).map_err(|err| err.to_string())?;
        Ok(None)
    });
    harness_rs::register("clear", move |_| {
        epd.clear().map_err(|err| err.to_string())?;
        Ok(None)
    });
    let pattern = jobs.clone();
    harness_rs::register("pattern", move |_| {
        pattern.send(Job::Pattern);
        Ok(None)
    });
    let upload = Mutex::new(None::<Vec<u8>>);
    harness_rs::register("img", move |args| img(&upload, &jobs, args));
}

fn img(upload: &Mutex<Option<Vec<u8>>>, jobs: &idf::sync::Sender<Job>, args: &[&str]) -> Reply {
    let mut upload = upload.lock();
    match args {
        ["begin", bytes] => {
            let bytes: usize = bytes.parse().ok().filter(|&n| n > 0 && n <= IMG_MAX).ok_or("bad size")?;
            let mut data = Vec::new();
            data.try_reserve_exact(bytes).map_err(|_| "no mem")?;
            *upload = Some(data);
            Ok(Some("begin".to_string()))
        }
        ["data", b64] => {
            let data = upload.as_mut().ok_or("no upload in progress")?;
            if !base64(b64, data) {
                *upload = None;
                return Err("bad data or overflow".to_string());
            }
            Ok(Some(format!("data {}", data.len())))
        }
        ["end"] => {
            let data = upload.take().ok_or("no upload in progress")?;
            if data.len() != data.capacity() {
                return Err("size mismatch".to_string());
            }
            let (done, result) = channel(1);
            jobs.send(Job::Image(data, done));
            result.recv().map(|(w, h)| Some(format!("{w}x{h}")))
        }
        ["abort"] => {
            *upload = None;
            Ok(Some("abort".to_string()))
        }
        _ => Err("usage: img <begin <bytes>|data <base64>|end|abort>".to_string()),
    }
}

/// Appends what `text` decodes to, without growing `data` past its capacity.
fn base64(text: &str, data: &mut Vec<u8>) -> bool {
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => continue,
            _ => return false,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if data.len() == data.capacity() {
                return false;
            }
            data.push((acc >> bits) as u8);
        }
    }
    true
}

#[cfg(all(target_os = "none", CONFIG_BSP_RS_DRIVER_EPD_LL))]
fn waveforms(display: &'static Display) {
    use bsp_rs::epd_ll::{STEP_MAX, Slot, Waveforms};

    struct Upload {
        slot: usize,
        steps: usize,
        words: Vec<u32>,
    }

    const NAMES: [&str; 4] = ["fast", "quality", "text", "clear"];
    const BY_INDEX: [Slot; 4] = [Slot::Fast, Slot::Quality, Slot::Text, Slot::Clear];

    fn slot(name: &str) -> Option<usize> {
        name.parse().ok().filter(|&i: &usize| i < NAMES.len()).or_else(|| NAMES.iter().position(|&s| s == name))
    }

    fn hex_words(hex: &str, out: &mut Vec<u32>) -> bool {
        if hex.len() % 8 != 0 {
            return false;
        }
        hex.as_bytes().chunks_exact(8).all(|digits| {
            let word = core::str::from_utf8(digits).ok().and_then(|d| u32::from_str_radix(d, 16).ok());
            word.map(|w| out.push(w)).is_some()
        })
    }

    let waveforms = Waveforms::of(display);
    harness_rs::register("tweak", |_| Ok(Some(format!("{STEP_MAX}"))));
    harness_rs::register("dump", move |args| {
        let slot = args.first().and_then(|s| slot(s)).ok_or("usage: dump <slot>")?;
        let lut = waveforms.get(BY_INDEX[slot]).map_err(|err| err.to_string())?;
        let mut text = format!("{slot} {} ", lut.len());
        for word in lut.iter().flatten() {
            text.push_str(&format!("{word:08x}"));
        }
        Ok(Some(text))
    });
    let upload = Mutex::new(None::<Upload>);
    harness_rs::register("wf", move |args| {
        let mut upload = upload.lock();
        match *args {
            ["begin", slot_name, steps] => {
                let slot = slot(slot_name).ok_or("bad slot")?;
                let steps: usize = steps.parse().ok().filter(|&n| (1..=STEP_MAX).contains(&n)).ok_or("bad steps")?;
                *upload = Some(Upload { slot, steps, words: Vec::with_capacity(steps * 16) });
                Ok(Some("begin".to_string()))
            }
            ["data", hex] => {
                let up = upload.as_mut().ok_or("no upload in progress")?;
                if !hex_words(hex, &mut up.words) || up.words.len() > up.steps * 16 {
                    *upload = None;
                    return Err("bad hex or overflow".to_string());
                }
                Ok(Some(format!("data {}", up.words.len())))
            }
            ["end"] => {
                let up = upload.take().ok_or("no upload in progress")?;
                if up.words.len() != up.steps * 16 {
                    return Err("length mismatch".to_string());
                }
                let lut = up.words.chunks_exact(16).map(|f| <[u32; 16]>::try_from(f).unwrap_or_default()).collect();
                waveforms.set(BY_INDEX[up.slot], lut).map_err(|err| err.to_string())?;
                Ok(Some(format!("{} {}", up.slot, up.steps)))
            }
            ["abort"] => {
                *upload = None;
                Ok(Some("abort".to_string()))
            }
            _ => Err("usage: wf <begin <slot> <steps>|data <hex>|end|abort>".to_string()),
        }
    });
}

#[cfg(not(all(target_os = "none", CONFIG_BSP_RS_DRIVER_EPD_LL)))]
fn waveforms(_display: &'static Display) {
    for name in ["tweak", "dump", "wf"] {
        harness_rs::register(name, |_| Err("unsupported".to_string()));
    }
}
