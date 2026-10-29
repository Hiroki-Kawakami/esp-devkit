/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The harness line protocol (docs/harness.md) on the host's stdin/stdout. The
 * host answers what it can see itself and forwards the rest to the firmware.
 */

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use jpeg_encoder::{ColorType, Encoder};

use crate::device::Device;
use crate::protocol::PixelFormat;

const DEFAULT_QUALITY: u8 = 90;
const B64_LINE_RAW: usize = 768;

fn reply(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "#{line}").and_then(|_| out.flush());
}

/// Returns at stdin EOF or `quit`; `true` for `quit`.
pub fn serve(device: &Arc<Device>) -> bool {
    for line in io::stdin().lock().lines().map_while(Result::ok) {
        let args: Vec<&str> = line.split_whitespace().collect();
        let Some(&cmd) = args.first() else { continue };
        match cmd {
            "ping" => {
                if device.ready() {
                    reply(&format!("OK {}", args.join(" ")));
                }
            }
            "info" => info(device),
            "down" | "move" => match parse_contact(&args[1..]) {
                Some((id, x, y)) => {
                    device.touch(id, Some((x, y)));
                    reply(&format!("OK {cmd}"));
                }
                None => reply(&format!("ERR {cmd} usage: {cmd} <id> <x> <y>")),
            },
            "up" => {
                device.touch(args.get(1).and_then(|a| a.parse().ok()).unwrap_or(0), None);
                reply("OK up");
            }
            "idle" => reply("OK idle 1"),
            "snap" => snap(device, args.get(1).and_then(|a| a.parse().ok()).unwrap_or(0)),
            "reset" => match device.start() {
                Ok(()) => reply("OK reset"),
                Err(err) => reply(&format!("ERR reset {err}")),
            },
            "quit" => {
                reply("OK quit");
                return true;
            }
            _ => device.write_stdin(&line),
        }
    }
    false
}

fn parse_contact(args: &[&str]) -> Option<(u8, u16, u16)> {
    match args {
        [id, x, y] => Some((id.parse().ok()?, x.parse().ok()?, y.parse().ok()?)),
        _ => None,
    }
}

fn info(device: &Device) {
    let Some(p) = device.present() else {
        reply("ERR info nothing presented yet");
        return;
    };
    let format = match p.format {
        PixelFormat::L8 => "l8",
        PixelFormat::Rgb565 => "rgb565",
        PixelFormat::Bgr888 => "rgb888",
    };
    reply(&format!("OK info {} {} {format} 1 0 1 0", p.width, p.height));
}

fn snap(device: &Device, quality: u8) {
    let Some(frame) = device.frame() else {
        reply("ERR snap nothing presented yet");
        return;
    };
    let mut jpeg = Vec::new();
    let quality = if quality == 0 { DEFAULT_QUALITY } else { quality.min(100) };
    let encoded = Encoder::new(&mut jpeg, quality).encode(&frame.rgb, frame.width as u16, frame.height as u16, ColorType::Rgb);
    if let Err(err) = encoded {
        reply(&format!("ERR snap {err}"));
        return;
    }
    reply(&format!("OK snap {} {}", frame.width, frame.height));
    for chunk in jpeg.chunks(B64_LINE_RAW) {
        reply(&format!("D {}", base64(chunk)));
    }
    reply(&format!("END {}", jpeg.len()));
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let v = u32::from(c[0]) << 16 | u32::from(*c.get(1).unwrap_or(&0)) << 8 | u32::from(*c.get(2).unwrap_or(&0));
        out.push(TABLE[(v >> 18) as usize & 63] as char);
        out.push(TABLE[(v >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { TABLE[(v >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { TABLE[v as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
