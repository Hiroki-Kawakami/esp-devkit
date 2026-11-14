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
use crate::text::{TOFU, draw_tofu, tofu_size};

const EMPTY: u16 = u16::MAX;
const BYTES_PER_SLOT: usize = 128;

#[derive(Clone, Copy)]
struct Slot {
    font: *const Font,
    glyph: u32,
    offset: u32,
    len: u32,
    frame: u32,
}

/// Decoded A8 glyph masks in a ring of `bytes`, the oldest not used in this
/// frame evicted first. A mask
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
        let empty = Slot { font: ptr::null(), glyph: 0, offset: 0, len: 0, frame: 0 };
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

    fn index_of(&self, slot: usize) -> usize {
        let mask = self.mask();
        let s = &self.slots[slot];
        let mut i = hash(s.font, s.glyph) & mask;
        while usize::from(self.index[i]) != slot {
            i = (i + 1) & mask;
        }
        i
    }

    fn unindex(&mut self, slot: usize) {
        let mask = self.mask();
        let mut i = self.index_of(slot);
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

    fn evict_oldest(&mut self) {
        self.unindex(self.head);
        self.head = (self.head + 1) % self.slots.len();
        self.count -= 1;
    }

    // The mask was handed out this frame and cannot move, so the ring skips
    // over it and it becomes the newest.
    fn requeue_oldest(&mut self) {
        let s = self.slots[self.head];
        let at = (self.head + self.count) % self.slots.len();
        if at != self.head {
            let i = self.index_of(self.head);
            self.index[i] = at as u16;
            self.slots[at] = s;
        }
        self.head = (self.head + 1) % self.slots.len();
        self.write = (s.offset + s.len) as usize;
    }

    fn reserve(&mut self, len: usize) -> Option<usize> {
        if len > self.data.len() {
            return None;
        }
        let mut requeued = 0;
        loop {
            let blocked = self.count == self.slots.len()
                || self.count > 0 && {
                    let start = self.slots[self.head].offset as usize;
                    if self.write + len > self.data.len() { start >= self.write || start < len } else { start >= self.write && start < self.write + len }
                };
            if !blocked {
                break;
            }
            if self.slots[self.head].frame != self.frame {
                self.evict_oldest();
            } else if requeued < self.count {
                self.requeue_oldest();
                requeued += 1;
            } else {
                return None;
            }
        }
        let offset = if self.write + len > self.data.len() { 0 } else { self.write };
        self.write = offset + len;
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
        let (width, height) = if glyph & TOFU != 0 {
            tofu_size(font, glyph)
        } else {
            let record = font.glyph(glyph);
            (record.width, record.height)
        };
        let len = usize::from(width) * usize::from(height);
        let offset = self.reserve(len)?;
        let dst = &mut self.data[offset..offset + len];
        if glyph & TOFU != 0 {
            draw_tofu(font, glyph, dst);
        } else {
            font.decode(glyph, dst);
        }
        self.decoded = self.decoded.wrapping_add(1);

        let at = (self.head + self.count) % self.slots.len();
        self.slots[at] = Slot { font: key, glyph, offset: offset as u32, len: len as u32, frame: self.frame };
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
