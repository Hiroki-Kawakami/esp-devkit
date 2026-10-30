/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

pub const COOKIE_BYTES: usize = 24;

const ATOM_HEADER_BYTES: usize = 12;
const FULL_BOX_BYTES: usize = 4;

fn be32(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}

fn atom_is(data: &[u8], name: &[u8; 4]) -> bool {
    data.len() >= ATOM_HEADER_BYTES && data[4..8] == *name
}

/// The ALACSpecificConfig in a magic cookie, behind any `frma` and `alac` atom
/// headers or the version and flags of an MP4 `alac` box.
pub fn find_cookie(mut data: &[u8]) -> Option<&[u8]> {
    if atom_is(data, b"frma") {
        data = &data[ATOM_HEADER_BYTES..];
    }
    if atom_is(data, b"alac") {
        data = &data[ATOM_HEADER_BYTES..];
    } else if data.len() >= FULL_BOX_BYTES + COOKIE_BYTES && be32(data) == 0 {
        data = &data[FULL_BOX_BYTES..];
    }
    if data.len() < COOKIE_BYTES || be32(data) == 0 {
        return None;
    }
    Some(&data[..COOKIE_BYTES])
}

/// Sample rate, channels and bits per sample.
pub fn cookie_format(cookie: &[u8]) -> (u32, u8, u8) {
    (be32(&cookie[20..]), cookie[9], cookie[5])
}
