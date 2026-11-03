/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Allocator;

use crate::geom::{Corners, Rect};
use crate::math::turns;

const SUBSAMPLES: i32 = 4;
const BOX_PASSES: usize = 3;

/// Antialiased corners of a rounded rectangle. Masks are never freed: a UI has
/// a handful of radii, so keep them with the theme.
#[derive(Clone, Copy)]
pub struct CornerMask {
    radius: u16,
    corners: Corners,
    data: &'static [u8],
}

impl CornerMask {
    pub fn new_in<A: Allocator + 'static>(radius: u16, alloc: A) -> Self {
        let r = i32::from(radius);
        let width = usize::from(radius) * 2;
        let mut data = Vec::with_capacity_in(width * usize::from(radius), alloc);
        let limit = (r * SUBSAMPLES * 2) * (r * SUBSAMPLES * 2);
        for y in 0..r {
            let mut row = [0u8; 256];
            for x in 0..r {
                let mut inside = 0;
                for sy in 0..SUBSAMPLES {
                    for sx in 0..SUBSAMPLES {
                        let dx = (r - x) * SUBSAMPLES * 2 - sx * 2 - 1;
                        let dy = (r - y) * SUBSAMPLES * 2 - sy * 2 - 1;
                        inside += i32::from(dx * dx + dy * dy <= limit);
                    }
                }
                row[x as usize] = (inside * 255 / (SUBSAMPLES * SUBSAMPLES)) as u8;
            }
            data.extend_from_slice(&row[..usize::from(radius)]);
            data.extend(row[..usize::from(radius)].iter().rev());
        }
        Self { radius, corners: Corners::ALL, data: data.leak() }
    }

    /// The corners of a rounded outline `stroke` px wide.
    pub fn ring_in<A: Allocator + 'static>(radius: u16, stroke: u16, alloc: A) -> Self {
        let r = i32::from(radius);
        let inner = (r - i32::from(stroke)).max(0);
        let width = usize::from(radius) * 2;
        let mut data = Vec::with_capacity_in(width * usize::from(radius), alloc);
        let outer_limit = (r * SUBSAMPLES * 2) * (r * SUBSAMPLES * 2);
        let inner_limit = (inner * SUBSAMPLES * 2) * (inner * SUBSAMPLES * 2);
        for y in 0..r {
            let mut row = [0u8; 256];
            for x in 0..r {
                let mut inside = 0;
                for sy in 0..SUBSAMPLES {
                    for sx in 0..SUBSAMPLES {
                        let dx = (r - x) * SUBSAMPLES * 2 - sx * 2 - 1;
                        let dy = (r - y) * SUBSAMPLES * 2 - sy * 2 - 1;
                        let distance = dx * dx + dy * dy;
                        inside += i32::from(distance <= outer_limit && distance > inner_limit);
                    }
                }
                row[x as usize] = (inside * 255 / (SUBSAMPLES * SUBSAMPLES)) as u8;
            }
            data.extend_from_slice(&row[..usize::from(radius)]);
            data.extend(row[..usize::from(radius)].iter().rev());
        }
        Self { radius, corners: Corners::ALL, data: data.leak() }
    }

    /// The same mask rounding only `corners`; the others stay square.
    pub const fn only(self, corners: Corners) -> Self {
        Self { corners, ..self }
    }

    pub const fn radius(&self) -> u16 {
        self.radius
    }

    pub const fn corners(&self) -> Corners {
        self.corners
    }

    pub(crate) fn data(&self) -> *const u8 {
        self.data.as_ptr()
    }

    pub(crate) fn coverage(&self, rect: &Rect, x: i16, y: i16) -> u8 {
        let r = self.radius as i16;
        let (column, right) = if x < rect.x + r {
            (x - rect.x, false)
        } else if x >= rect.right() - r {
            (r + x - (rect.right() - r), true)
        } else {
            return 255;
        };
        let (row, low) = if y < rect.y + r {
            (y - rect.y, false)
        } else if y >= rect.bottom() - r {
            (rect.bottom() - 1 - y, true)
        } else {
            return 255;
        };
        if !self.corners.contains(corner_at(right, low)) {
            return 255;
        }
        self.data[row as usize * usize::from(self.radius) * 2 + column as usize]
    }
}

pub(crate) const fn corner_at(right: bool, low: bool) -> Corners {
    match (right, low) {
        (false, false) => Corners::TOP_LEFT,
        (true, false) => Corners::TOP_RIGHT,
        (false, true) => Corners::BOTTOM_LEFT,
        (true, true) => Corners::BOTTOM_RIGHT,
    }
}

/// A ring `stroke` px wide inside a circle of `radius`, with the angle of each
/// pixel, for arcs. Like [`CornerMask`] it is never freed.
#[derive(Clone, Copy)]
pub struct ArcMask {
    radius: u16,
    stroke: u16,
    data: &'static [u8],
}

impl ArcMask {
    pub fn new_in<A: Allocator + 'static>(radius: u16, stroke: u16, alloc: A) -> Self {
        let r = i32::from(radius);
        let side = usize::from(radius) * 2;
        let inner = (r - i32::from(stroke)).max(0);
        let outer_limit = (r * SUBSAMPLES * 2) * (r * SUBSAMPLES * 2);
        let inner_limit = (inner * SUBSAMPLES * 2) * (inner * SUBSAMPLES * 2);
        let mut data = Vec::with_capacity_in(side * side * 3, alloc);
        for y in 0..side as i32 {
            for x in 0..side as i32 {
                let mut inside = 0;
                for sy in 0..SUBSAMPLES {
                    for sx in 0..SUBSAMPLES {
                        let dx = x * SUBSAMPLES * 2 + sx * 2 + 1 - r * SUBSAMPLES * 2;
                        let dy = y * SUBSAMPLES * 2 + sy * 2 + 1 - r * SUBSAMPLES * 2;
                        let distance = dx * dx + dy * dy;
                        inside += i32::from(distance <= outer_limit && distance > inner_limit);
                    }
                }
                let angle = (turns(x as f32 + 0.5 - r as f32, y as f32 + 0.5 - r as f32) * 65536.0) as u32 as u16;
                data.push((inside * 255 / (SUBSAMPLES * SUBSAMPLES)) as u8);
                data.extend_from_slice(&angle.to_le_bytes());
            }
        }
        Self { radius, stroke, data: data.leak() }
    }

    pub const fn radius(&self) -> u16 {
        self.radius
    }

    pub const fn stroke(&self) -> u16 {
        self.stroke
    }

    pub(crate) fn side(&self) -> usize {
        usize::from(self.radius) * 2
    }

    /// Coverage and angle of the pixel at `(x, y)` from the top left.
    #[inline(always)]
    pub(crate) fn at(&self, x: usize, y: usize) -> (u8, u16) {
        let i = (y * self.side() + x) * 3;
        (self.data[i], u16::from_le_bytes([self.data[i + 1], self.data[i + 2]]))
    }
}

/// The blurred outline of a rounded rectangle, drawn as nine slices. Like CSS
/// `box-shadow` it reaches `blur` px outside the rectangle, and like
/// [`CornerMask`] it is never freed.
#[derive(Clone, Copy)]
pub struct ShadowMask {
    blur: u16,
    margin: u16,
    data: &'static [u8],
    corners: Corners,
    square: Tile,
}

#[derive(Clone, Copy)]
pub(crate) struct Tile {
    pub(crate) margin: u16,
    pub(crate) data: &'static [u8],
}

impl Tile {
    pub(crate) fn side(&self) -> usize {
        usize::from(self.margin) * 2 + 1
    }
}

impl ShadowMask {
    pub fn new_in<A: Allocator + Clone + 'static>(blur: u16, radius: u16, alloc: A) -> Self {
        let margin = usize::from(blur) * 2 + usize::from(radius);
        let side = margin * 2 + 1;
        let mut data = vec::from_elem_in(0u8, side * side, alloc.clone());
        let inset = usize::from(blur);
        let inner = side - inset * 2;
        let r = i32::from(radius);
        for y in 0..inner {
            for x in 0..inner {
                let dx = (r - x.min(inner - 1 - x) as i32).max(0);
                let dy = (r - y.min(inner - 1 - y) as i32).max(0);
                if dx == 0 || dy == 0 || dx * dx + dy * dy <= r * r {
                    data[(y + inset) * side + x + inset] = 255;
                }
            }
        }
        let width = (usize::from(blur) * 94 / 100).max(1) | 1;
        let mut line = vec::from_elem_in(0u8, side, alloc);
        for _ in 0..BOX_PASSES {
            for y in 0..side {
                box_blur(&mut data, y * side, 1, side, width, &mut line);
            }
            for x in 0..side {
                box_blur(&mut data, x, side, side, width, &mut line);
            }
        }
        let data = data.leak();
        let margin = margin as u16;
        Self { blur, margin, data, corners: Corners::ALL, square: Tile { margin, data } }
    }

    /// The same shadow rounding only `corners`, taking the others from
    /// `square`, a mask of the same blur and no radius.
    pub fn only(self, corners: Corners, square: ShadowMask) -> Self {
        debug_assert!(square.blur == self.blur);
        Self { corners, square: Tile { margin: square.margin, data: square.data }, ..self }
    }

    pub const fn blur(&self) -> u16 {
        self.blur
    }

    pub(crate) const fn margin(&self) -> u16 {
        self.margin
    }

    pub(crate) fn side(&self) -> usize {
        usize::from(self.margin) * 2 + 1
    }

    pub(crate) fn data(&self) -> *const u8 {
        self.data.as_ptr()
    }

    pub(crate) fn at(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.side() + x]
    }

    pub(crate) fn tile(&self, corner: Corners) -> Tile {
        if self.corners.contains(corner) { Tile { margin: self.margin, data: self.data } } else { self.square }
    }
}

fn box_blur<A: Allocator>(data: &mut [u8], start: usize, step: usize, count: usize, width: usize, line: &mut Vec<u8, A>) {
    let half = width / 2;
    for (i, v) in line.iter_mut().enumerate().take(count) {
        *v = data[start + i * step];
    }
    let at = |i: isize| u32::from(line[i.clamp(0, count as isize - 1) as usize]);
    let mut sum: u32 = (-(half as isize)..=half as isize).map(at).sum();
    for i in 0..count {
        data[start + i * step] = ((sum + width as u32 / 2) / width as u32) as u8;
        sum += at((i + half + 1) as isize);
        sum -= at(i as isize - half as isize);
    }
}
