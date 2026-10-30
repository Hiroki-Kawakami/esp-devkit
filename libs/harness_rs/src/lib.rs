/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The harness line protocol on the console (docs/harness.md): the firmware
 * side of `devkit`, and usable by hand from any serial monitor.
 */

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod jpeg;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{CStr, c_int};

use bsp_rs::Board;
use bsp_rs::display::{Display, PixelFormat};
use bsp_rs::touch::{Point, Touch};
use idf::log::error;
use idf::sync::{LazyLock, Mutex};
use idf::thread::Builder;

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const TAG: &CStr = c"harness";
const LINE_MAX: usize = 512;
const STACK_SIZE: usize = 8192;
const PRIORITY: u32 = 3;
const B64_LINE_RAW: usize = 384;
const EOF: c_int = -1;

/// Runs on the harness task. `Ok(Some(text))` replies `#OK <name> <text>`,
/// `Ok(None)` `#OK <name>`, `Err(text)` `#ERR <name> <text>`.
pub type Handler = Arc<dyn Fn(&[&str]) -> Result<Option<String>, String> + Send + Sync>;

static HANDLERS: LazyLock<Mutex<BTreeMap<String, Handler>>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));
static IDLE: LazyLock<Mutex<Option<Arc<dyn Fn() -> bool + Send + Sync>>>> = LazyLock::new(|| Mutex::new(None));
static OUTPUT: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Adds an app command, replacing one of the same name.
pub fn register(name: &str, handler: impl Fn(&[&str]) -> Result<Option<String>, String> + Send + Sync + 'static) {
    HANDLERS.lock().insert(name.to_string(), Arc::new(handler));
}

/// What `idle` answers; without it the firmware always counts as idle.
pub fn set_idle(idle: impl Fn() -> bool + Send + Sync + 'static) {
    *IDLE.lock() = Some(Arc::new(idle));
}

/// Starts answering on the console.
pub fn start(board: &Board) -> idf::Result<()> {
    let harness = Harness { display: board.display, touch: board.touch, contacts: BTreeMap::new() };
    Builder::new().name("harness").stack_size(STACK_SIZE).priority(PRIORITY).spawn(move || harness.run())?;
    Ok(())
}

fn write(data: &[u8]) {
    let _turn = OUTPUT.lock();
    unsafe { ffi::harness_rs_console_write(data.as_ptr().cast(), data.len()) };
}

fn reply(text: &str) {
    let line = format!("#{text}\n");
    write(line.as_bytes());
}

struct Harness {
    display: Option<&'static Display>,
    touch: Option<&'static Touch>,
    contacts: BTreeMap<u8, (u16, u16)>,
}

impl Harness {
    fn run(mut self) {
        unsafe { ffi::harness_rs_console_init() };
        let mut line = Vec::with_capacity(LINE_MAX);
        loop {
            let c = unsafe { ffi::harness_rs_console_getc() };
            if c == EOF {
                #[cfg(not(target_os = "none"))]
                return;
                #[cfg(target_os = "none")]
                {
                    idf::thread::sleep(core::time::Duration::from_millis(20));
                    continue;
                }
            }
            match c as u8 {
                b'\n' | b'\r' => {
                    if !line.is_empty() {
                        self.handle(&String::from_utf8_lossy(&line));
                        line.clear();
                    }
                }
                b if line.len() < LINE_MAX => line.push(b),
                _ => line.clear(),
            }
        }
    }

    fn handle(&mut self, line: &str) {
        let args: Vec<&str> = line.split_whitespace().collect();
        let Some(&name) = args.first() else { return };
        if name.starts_with('#') {
            return;
        }
        let rest = &args[1..];
        let result = match name {
            "ping" => Ok(rest.first().map(|t| t.to_string())),
            "info" => self.info(),
            "down" | "move" => self.contact(rest),
            "up" => self.release(rest),
            "idle" => {
                let idle = IDLE.lock().clone();
                Ok(Some(if idle.is_none_or(|f| f()) { "1" } else { "0" }.to_string()))
            }
            "snap" => return self.snap(rest.first().and_then(|q| q.parse().ok()).unwrap_or(0)),
            "quit" => Ok(None),
            _ => {
                let handler = HANDLERS.lock().get(name).cloned();
                match handler {
                    Some(handler) => handler(rest),
                    None => {
                        reply(&format!("ERR unknown command: {name}"));
                        return;
                    }
                }
            }
        };
        match result {
            Ok(Some(text)) => reply(&format!("OK {name} {text}")),
            Ok(None) => reply(&format!("OK {name}")),
            Err(text) => reply(&format!("ERR {name} {text}")),
        }
    }

    fn info(&self) -> Result<Option<String>, String> {
        let display = self.display.ok_or("no display")?;
        let size = display.size();
        let format = match display.pixel_format() {
            PixelFormat::L8 => "l8",
            PixelFormat::Rgb565 => "rgb565",
            PixelFormat::Rgb888 => "rgb888",
        };
        let touch = u8::from(self.touch.is_some());
        Ok(Some(format!("{} {} {format} {touch} 0 1 0", size.width, size.height)))
    }

    fn contact(&mut self, args: &[&str]) -> Result<Option<String>, String> {
        let parsed = match args {
            [id, x, y] => id.parse().ok().zip(x.parse().ok()).zip(y.parse().ok()),
            _ => None,
        };
        let ((id, x), y) = parsed.ok_or("usage: <id> <x> <y>")?;
        self.contacts.insert(id, (x, y));
        self.inject()
    }

    fn release(&mut self, args: &[&str]) -> Result<Option<String>, String> {
        self.contacts.remove(&args.first().and_then(|a| a.parse().ok()).unwrap_or(0));
        self.inject()
    }

    fn inject(&self) -> Result<Option<String>, String> {
        let touch = self.touch.ok_or("no touch")?;
        let points: Vec<Point> = self.contacts.iter().map(|(&id, &(x, y))| Point { x, y, id }).collect();
        touch.inject(&points);
        Ok(None)
    }

    fn snap(&self, quality: u8) {
        let Some(display) = self.display else {
            reply("ERR snap no display");
            return;
        };
        let size = display.size();
        reply(&format!("OK snap {} {}", size.width, size.height));
        let mut pending: Vec<u8> = Vec::with_capacity(B64_LINE_RAW);
        let mut total = 0usize;
        let result = jpeg::encode(display, quality, &mut |data| {
            total += data.len();
            for chunk in data.chunks(B64_LINE_RAW) {
                pending.extend_from_slice(chunk);
                while pending.len() >= B64_LINE_RAW {
                    data_line(&pending[..B64_LINE_RAW]);
                    pending.drain(..B64_LINE_RAW);
                }
            }
            true
        });
        if !pending.is_empty() {
            data_line(&pending);
        }
        match result {
            Ok(()) => reply(&format!("END {total}")),
            Err(err) => {
                error!(TAG, "snap: {err}");
                reply(&format!("ERR snap {err}"));
            }
        }
    }
}

fn data_line(raw: &[u8]) {
    let mut line = String::with_capacity(raw.len().div_ceil(3) * 4 + 4);
    line.push_str("#D ");
    base64(raw, &mut line);
    line.push('\n');
    write(line.as_bytes());
}

fn base64(data: &[u8], out: &mut String) {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for c in data.chunks(3) {
        let v = u32::from(c[0]) << 16 | u32::from(*c.get(1).unwrap_or(&0)) << 8 | u32::from(*c.get(2).unwrap_or(&0));
        out.push(TABLE[(v >> 18) as usize & 63] as char);
        out.push(TABLE[(v >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { TABLE[(v >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { TABLE[v as usize & 63] as char } else { '=' });
    }
}
