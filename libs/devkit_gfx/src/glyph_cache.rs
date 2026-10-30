/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::alloc::Global;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Allocator;
use core::ptr;

use crate::font::Font;

const EMPTY: u16 = u16::MAX;
const BYTES_PER_SLOT: usize = 128;

#[derive(Clone, Copy)]
struct Slot {
    font: *const Font,
    glyph: u32,
    offset: u32,
    frame: u32,
}

/// Decoded A8 glyph masks in a ring of `bytes`, oldest evicted first. A mask
/// stays valid until the next `begin_frame`; a frame's glyphs must fit the ring.
pub struct GlyphCache<A: Allocator + Clone = Global> {
    data: Vec<u8, A>,
    slots: Vec<Slot, A>,
    index: Vec<u16, A>,
    head: usize,
    count: usize,
    write: usize,
    frame: u32,
    pub(crate) decoded: u32,
}

unsafe impl<A: Allocator + Clone + Send> Send for GlyphCache<A> {}

fn hash(font: *const Font, glyph: u32) -> usize {
    let mixed = (font as usize as u32).wrapping_mul(0x9E37_79B1) ^ glyph.wrapping_mul(0x85EB_CA77);
    (mixed ^ mixed >> 15) as usize
}

impl GlyphCache<Global> {
    pub fn new(bytes: usize) -> Self {
        Self::new_in(bytes, Global)
    }
}

impl<A: Allocator + Clone> GlyphCache<A> {
    pub fn new_in(bytes: usize, alloc: A) -> Self {
        let slot_count = (bytes / BYTES_PER_SLOT).clamp(16, EMPTY as usize / 2);
        let empty = Slot { font: ptr::null(), glyph: 0, offset: 0, frame: 0 };
        Self {
            data: vec::from_elem_in(0, bytes, alloc.clone()),
            slots: vec::from_elem_in(empty, slot_count, alloc.clone()),
            index: vec::from_elem_in(EMPTY, (slot_count * 2).next_power_of_two(), alloc),
            head: 0,
            count: 0,
            write: 0,
            frame: 0,
            decoded: 0,
        }
    }

    /// Glyphs decoded so far, for measuring misses.
    pub fn decoded(&self) -> u32 {
        self.decoded
    }

    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    fn mask(&self) -> usize {
        self.index.len() - 1
    }

    fn lookup(&self, font: *const Font, glyph: u32) -> Option<usize> {
        let mask = self.mask();
        let mut i = hash(font, glyph) & mask;
        loop {
            let slot = self.index[i];
            if slot == EMPTY {
                return None;
            }
            let s = &self.slots[usize::from(slot)];
            if ptr::eq(s.font, font) && s.glyph == glyph {
                return Some(usize::from(slot));
            }
            i = (i + 1) & mask;
        }
    }

    fn unindex(&mut self, slot: usize) {
        let mask = self.mask();
        let s = self.slots[slot];
        let mut i = hash(s.font, s.glyph) & mask;
        while usize::from(self.index[i]) != slot {
            i = (i + 1) & mask;
        }
        // Backward shift, so probes never meet a hole.
        let mut next = (i + 1) & mask;
        while self.index[next] != EMPTY {
            let other = self.slots[usize::from(self.index[next])];
            let home = hash(other.font, other.glyph) & mask;
            if (next.wrapping_sub(home) & mask) >= (next.wrapping_sub(i) & mask) {
                self.index[i] = self.index[next];
                i = next;
            }
            next = (next + 1) & mask;
        }
        self.index[i] = EMPTY;
    }

    fn evict_oldest(&mut self) -> bool {
        if self.slots[self.head].frame == self.frame {
            return false;
        }
        self.unindex(self.head);
        self.head = (self.head + 1) % self.slots.len();
        self.count -= 1;
        true
    }

    fn reserve(&mut self, len: usize) -> Option<usize> {
        if len > self.data.len() {
            return None;
        }
        if self.write + len > self.data.len() {
            while self.count > 0 && self.slots[self.head].offset as usize >= self.write {
                if !self.evict_oldest() {
                    return None;
                }
            }
            self.write = 0;
        }
        while self.count > 0 {
            let start = self.slots[self.head].offset as usize;
            if start < self.write || start >= self.write + len {
                break;
            }
            if !self.evict_oldest() {
                return None;
            }
        }
        let offset = self.write;
        self.write += len;
        Some(offset)
    }

    /// The glyph's mask, decoded on a miss; `None` when the frame's glyphs
    /// already fill the cache.
    pub fn get(&mut self, font: &'static Font, glyph: u32) -> Option<*const u8> {
        let key: *const Font = font;
        if let Some(slot) = self.lookup(key, glyph) {
            self.slots[slot].frame = self.frame;
            return Some(unsafe { self.data.as_ptr().add(self.slots[slot].offset as usize) });
        }
        let record = font.glyph(glyph);
        let len = usize::from(record.width) * usize::from(record.height);
        if self.count == self.slots.len() && !self.evict_oldest() {
            return None;
        }
        let offset = self.reserve(len)?;
        font.decode(glyph, &mut self.data[offset..offset + len]);
        self.decoded = self.decoded.wrapping_add(1);

        let at = (self.head + self.count) % self.slots.len();
        self.slots[at] = Slot { font: key, glyph, offset: offset as u32, frame: self.frame };
        self.count += 1;
        let mask = self.mask();
        let mut i = hash(key, glyph) & mask;
        while self.index[i] != EMPTY {
            i = (i + 1) & mask;
        }
        self.index[i] = at as u16;
        Some(unsafe { self.data.as_ptr().add(offset) })
    }
}
