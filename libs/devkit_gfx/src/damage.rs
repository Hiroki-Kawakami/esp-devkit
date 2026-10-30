/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::geom::Rect;

const FULL_PERCENT: u32 = 75;

/// Up to `N` disjoint-ish rectangles to repaint. Rectangles that would exceed
/// `N` merge with their cheapest neighbour, and several covering 75% of `full`
/// promote to all of `full`.
#[derive(Clone, Copy)]
pub struct Damage<const N: usize> {
    full: Rect,
    regions: [Rect; N],
    count: usize,
}

impl<const N: usize> Damage<N> {
    pub const fn new(full: Rect) -> Self {
        Self { full, regions: [Rect::new(0, 0, 0, 0); N], count: 0 }
    }

    pub fn clear(&mut self) {
        self.count = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn regions(&self) -> &[Rect] {
        &self.regions[..self.count]
    }

    pub fn add_full(&mut self) {
        self.regions[0] = self.full;
        self.count = 1;
    }

    pub fn add(&mut self, rect: Rect) {
        let mut rect = rect.intersect(&self.full);
        if rect.is_empty() {
            return;
        }
        let mut i = 0;
        while i < self.count {
            if self.regions[i].intersects(&rect) {
                rect = rect.union(&self.regions[i]);
                self.count -= 1;
                self.regions[i] = self.regions[self.count];
                i = 0;
            } else {
                i += 1;
            }
        }
        if self.count == N {
            self.merge_cheapest(&mut rect);
        }
        self.regions[self.count] = rect;
        self.count += 1;
        let area: u32 = self.regions().iter().map(Rect::area).sum();
        if self.count > 1 && area * 100 >= self.full.area() * FULL_PERCENT {
            self.add_full();
        }
    }

    fn merge_cheapest(&mut self, rect: &mut Rect) {
        let mut best = 0;
        let mut best_cost = u32::MAX;
        for (i, region) in self.regions[..self.count].iter().enumerate() {
            let cost = region.union(rect).area() - region.area() - rect.area();
            if cost < best_cost {
                best = i;
                best_cost = cost;
            }
        }
        *rect = rect.union(&self.regions[best]);
        self.count -= 1;
        self.regions[best] = self.regions[self.count];
        let merged = *rect;
        let mut i = 0;
        while i < self.count {
            if self.regions[i].intersects(&merged) {
                *rect = rect.union(&self.regions[i]);
                self.count -= 1;
                self.regions[i] = self.regions[self.count];
            } else {
                i += 1;
            }
        }
    }
}
