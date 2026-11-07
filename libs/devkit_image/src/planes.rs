/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::pixel::Ycc;

/// How a decoder's YCbCr planes are sampled: the chroma planes, both alike,
/// take one sample per `1 << shift.0` pixels across and `1 << shift.1` down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlaneLayout {
    pub width: u16,
    pub height: u16,
    pub shift: (u32, u32),
    pub(crate) ycc: Ycc,
}

impl PlaneLayout {
    pub const fn chroma_width(&self) -> u16 {
        self.width.div_ceil(1 << self.shift.0)
    }

    pub const fn chroma_height(&self) -> u16 {
        self.height.div_ceil(1 << self.shift.1)
    }
}

/// Consecutive rows of one plane, from row `first` of the plane on.
#[derive(Clone, Copy)]
pub(crate) struct PlaneRows<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) stride: usize,
    pub(crate) first: usize,
    pub(crate) count: usize,
}

impl<'a> PlaneRows<'a> {
    pub(crate) fn row(&self, index: usize) -> &'a [u8] {
        &self.data[(index - self.first) * self.stride..]
    }

    pub(crate) fn end(&self) -> usize {
        self.first + self.count
    }
}

/// The samples a decoder finished since the last call, Y, Cb and Cr.
pub struct Planes<'a> {
    pub(crate) planes: [PlaneRows<'a>; 3],
}
