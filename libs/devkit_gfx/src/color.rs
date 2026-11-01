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

    /// `amount` of the way to `other`, 255 being all of it.
    pub const fn mix(self, other: Self, amount: u8) -> Self {
        Self {
            r: lerp8(self.r, other.r, amount),
            g: lerp8(self.g, other.g, amount),
            b: lerp8(self.b, other.b, amount),
            a: lerp8(self.a, other.a, amount),
        }
    }
}

const fn lerp8(a: u8, b: u8, amount: u8) -> u8 {
    (a as i32 + (b as i32 - a as i32) * amount as i32 / 255) as u8
}

pub(crate) const fn mul8(a: u8, b: u8) -> u8 {
    let v = a as u32 * b as u32 + 128;
    ((v + (v >> 8)) >> 8) as u8
}
