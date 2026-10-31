/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: i16,
    pub y: i16,
    pub w: i16,
    pub h: i16,
}

impl Rect {
    pub const fn new(x: i16, y: i16, w: i16, h: i16) -> Self {
        Self { x, y, w, h }
    }

    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub const fn right(&self) -> i16 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> i16 {
        self.y + self.h
    }

    pub const fn area(&self) -> u32 {
        if self.is_empty() { 0 } else { self.w as u32 * self.h as u32 }
    }

    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect::new(x, y, self.right().max(other.right()) - x, self.bottom().max(other.bottom()) - y)
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Rect::new(x, y, self.right().min(other.right()) - x, self.bottom().min(other.bottom()) - y)
    }

    /// What is left of `self` outside `hole`, as up to four rectangles.
    pub fn subtract(&self, hole: &Rect) -> impl Iterator<Item = Rect> + use<> {
        let cut = self.intersect(hole);
        let pieces = if cut.is_empty() {
            [*self, Rect::default(), Rect::default(), Rect::default()]
        } else {
            [
                Rect::new(self.x, self.y, self.w, cut.y - self.y),
                Rect::new(self.x, cut.y, cut.x - self.x, cut.h),
                Rect::new(cut.right(), cut.y, self.right() - cut.right(), cut.h),
                Rect::new(self.x, cut.bottom(), self.w, self.bottom() - cut.bottom()),
            ]
        };
        pieces.into_iter().filter(|r| !r.is_empty())
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        !self.intersect(other).is_empty()
    }

    pub fn contains(&self, other: &Rect) -> bool {
        self.x <= other.x && self.y <= other.y && self.right() >= other.right() && self.bottom() >= other.bottom()
    }

    pub const fn offset(&self, dx: i16, dy: i16) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    pub const fn expand(&self, by: i16) -> Rect {
        Rect::new(self.x - by, self.y - by, self.w + by * 2, self.h + by * 2)
    }
}
