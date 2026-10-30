/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// `0xRRGGBB`.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    /// Scales the alpha by `opacity`, 255 keeping it.
    pub const fn with_opacity(self, opacity: u8) -> Self {
        Self { a: mul8(self.a, opacity), ..self }
    }
}

pub(crate) const fn mul8(a: u8, b: u8) -> u8 {
    let v = a as u32 * b as u32 + 128;
    ((v + (v >> 8)) >> 8) as u8
}
