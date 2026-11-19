/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

pub(crate) const HEADER: usize = 9;

pub(crate) const DATA: u8 = 0;
pub(crate) const HEADERS: u8 = 1;
pub(crate) const PRIORITY: u8 = 2;
pub(crate) const RST_STREAM: u8 = 3;
pub(crate) const SETTINGS: u8 = 4;
pub(crate) const PUSH_PROMISE: u8 = 5;
pub(crate) const PING: u8 = 6;
pub(crate) const GOAWAY: u8 = 7;
pub(crate) const WINDOW_UPDATE: u8 = 8;
pub(crate) const CONTINUATION: u8 = 9;

pub(crate) const END_STREAM: u8 = 0x1;
pub(crate) const ACK: u8 = 0x1;
pub(crate) const END_HEADERS: u8 = 0x4;
pub(crate) const PADDED: u8 = 0x8;
pub(crate) const PRIORITY_FLAG: u8 = 0x20;

pub(crate) const SETTINGS_HEADER_TABLE_SIZE: u16 = 1;
pub(crate) const SETTINGS_ENABLE_PUSH: u16 = 2;
pub(crate) const SETTINGS_MAX_CONCURRENT_STREAMS: u16 = 3;
pub(crate) const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 4;
pub(crate) const SETTINGS_MAX_FRAME_SIZE: u16 = 5;
pub(crate) const SETTINGS_MAX_HEADER_LIST_SIZE: u16 = 6;

pub(crate) const PROTOCOL_ERROR: u32 = 1;
pub(crate) const CANCEL: u32 = 8;

pub(crate) fn header(out: &mut Vec<u8>, len: usize, kind: u8, flags: u8, stream: u32) {
    out.extend_from_slice(&(len as u32).to_be_bytes()[1..]);
    out.push(kind);
    out.push(flags);
    out.extend_from_slice(&(stream & 0x7FFF_FFFF).to_be_bytes());
}

/// Length, type, flags and stream of the frame `buf` starts with.
pub(crate) fn parse(buf: &[u8]) -> (usize, u8, u8, u32) {
    let len = (usize::from(buf[0]) << 16) | (usize::from(buf[1]) << 8) | usize::from(buf[2]);
    (len, buf[3], buf[4], u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7FFF_FFFF)
}

pub(crate) fn settings(out: &mut Vec<u8>, entries: &[(u16, u32)]) {
    header(out, entries.len() * 6, SETTINGS, 0, 0);
    for &(id, value) in entries {
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&value.to_be_bytes());
    }
}

pub(crate) fn window_update(out: &mut Vec<u8>, stream: u32, increment: u32) {
    header(out, 4, WINDOW_UPDATE, 0, stream);
    out.extend_from_slice(&increment.to_be_bytes());
}

pub(crate) fn rst_stream(out: &mut Vec<u8>, stream: u32, code: u32) {
    header(out, 4, RST_STREAM, 0, stream);
    out.extend_from_slice(&code.to_be_bytes());
}

pub(crate) fn goaway(out: &mut Vec<u8>, last: u32, code: u32) {
    header(out, 8, GOAWAY, 0, 0);
    out.extend_from_slice(&last.to_be_bytes());
    out.extend_from_slice(&code.to_be_bytes());
}

/// HEADERS, then CONTINUATION frames for what does not fit in `max`.
pub(crate) fn headers(out: &mut Vec<u8>, stream: u32, block: &[u8], end_stream: bool, max: usize) {
    let mut pieces = block.chunks(max.max(1)).peekable();
    let mut kind = HEADERS;
    let mut flags = if end_stream { END_STREAM } else { 0 };
    loop {
        let piece = pieces.next().unwrap_or(&[]);
        let last = pieces.peek().is_none();
        header(out, piece.len(), kind, flags | if last { END_HEADERS } else { 0 }, stream);
        out.extend_from_slice(piece);
        if last {
            return;
        }
        kind = CONTINUATION;
        flags = 0;
    }
}

/// The payload without its padding, and how long the padding was.
pub(crate) fn unpad(flags: u8, payload: &[u8]) -> Option<(&[u8], usize)> {
    if flags & PADDED == 0 {
        return Some((payload, 0));
    }
    let pad = usize::from(*payload.first()?);
    let body = payload.get(1..payload.len().checked_sub(pad)?)?;
    Some((body, pad + 1))
}
