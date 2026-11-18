/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
mod fb;
mod gram;
#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
mod ppa;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::Any;
use core::ops::{Deref, DerefMut};

use bsp_rs::display::{self, FrameBuffer, ImageFormat, Rotation};
use devkit_gfx::{DisplayList, GlyphCache, Rect};
use idf::heap::Caps;
use idf::{Error, Result};

use crate::blit::NewBlitter;
use crate::external::Buffers;

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
pub use fb::FbPresenter;
pub(crate) use gram::GramPresenter;

pub(crate) const MAX_LANES: usize = 2;
pub(crate) const MAX_FRAME_BUFFERS: usize = 3;
pub(crate) const STRIPS_PER_LANE: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentMode {
    /// Draw off screen and flip at a frame boundary: no tearing. With three
    /// framebuffers drawing never waits for the boundary to start.
    Swap,
    /// Overwrite the framebuffer on the panel: the fastest response, and rows
    /// tear while they scroll.
    InPlace,
}

pub struct Config {
    /// For panels with framebuffers.
    pub mode: PresentMode,
    /// At most this many framebuffers of the display.
    pub frame_buffers: usize,
    pub rotation: Rotation,
    /// Tasks drawing strips, one per core.
    pub lanes: usize,
    pub priority: u32,
    /// Split evenly into two buffers per lane.
    pub strips: StripSource,
    /// The presenter's own lists. Memory that may stay on one heap for the whole
    /// UI; the default fits boards with PSRAM.
    pub data_caps: Caps,
    /// Never memory that can be RTC RAM, as `INTERNAL` alone can: lanes run PIE
    /// code, whose saved registers RTC RAM cannot take. `SIMD` alone avoids it,
    /// on a chip that has the vector unit and so such memory.
    pub lane_stack_caps: Caps,
}

#[cfg(CONFIG_SOC_SIMD_INSTRUCTION_SUPPORTED)]
const LANE_STACK_CAPS: Caps = Caps::SIMD;
#[cfg(not(CONFIG_SOC_SIMD_INSTRUCTION_SUPPORTED))]
const LANE_STACK_CAPS: Caps = crate::INTERNAL_RAM;

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: PresentMode::Swap,
            frame_buffers: MAX_FRAME_BUFFERS,
            rotation: Rotation::R0,
            lanes: MAX_LANES,
            priority: 10,
            strips: StripSource::Allocate { bytes: 4 * 64 * 1024, caps: Caps::DMA | Caps::INTERNAL },
            data_caps: Caps::SPIRAM,
            lane_stack_caps: LANE_STACK_CAPS,
        }
    }
}

pub enum StripSource {
    Allocate { bytes: usize, caps: Caps },
    Given(Strips),
}

/// One `render`, in microseconds; lane times are summed over the lanes.
#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub prepare_us: u32,
    pub draw_us: u32,
    pub flip_us: u32,
    pub raster_us: u32,
    pub stall_us: u32,
    pub strips: u32,
    pub glyphs_decoded: u32,
    pub glyphs_missing: u32,
}

#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub(crate) struct Line([u8; 64]);

struct OwnedStrips(Vec<Line, Caps>);

impl Deref for OwnedStrips {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.0.as_ptr().cast(), self.0.len() * size_of::<Line>()) }
    }
}

impl DerefMut for OwnedStrips {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.0.as_mut_ptr().cast(), self.0.len() * size_of::<Line>()) }
    }
}

/// The memory a presenter draws strips in, handed over with `set_strips`.
pub struct Strips {
    memory: Box<dyn AnyStrips>,
    ptr: *mut u8,
    len: usize,
}

unsafe impl Send for Strips {}

trait AnyStrips: Send {
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<M: DerefMut<Target = [u8]> + Send + 'static> AnyStrips for M {
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl Strips {
    pub fn new<M: DerefMut<Target = [u8]> + Send + 'static>(memory: M) -> Self {
        let mut memory = Box::new(memory);
        let bytes = DerefMut::deref_mut(&mut *memory);
        let (ptr, len) = (bytes.as_mut_ptr(), bytes.len());
        Self { memory, ptr, len }
    }

    /// `bytes` from the heap, freed with the value.
    pub fn allocate(bytes: usize, caps: Caps) -> Result<Self> {
        let lines = bytes / size_of::<Line>();
        let mut memory = Vec::new_in(caps);
        memory.try_reserve_exact(lines).map_err(|_| Error::NO_MEM)?;
        memory.resize(lines, Line([0; 64]));
        Ok(Self::new(OwnedStrips(memory)))
    }

    /// Gives back what `new` took, if it was an `M`.
    pub fn into_inner<M: DerefMut<Target = [u8]> + Send + 'static>(self) -> Option<M> {
        self.memory.into_any().downcast::<M>().ok().map(|m| *m)
    }

    /// The first byte on a cache line.
    pub(crate) fn aligned(&self) -> *mut u8 {
        unsafe { self.ptr.add(self.ptr.align_offset(size_of::<Line>())) }
    }

    /// Two aligned buffers per lane.
    pub(crate) fn buffers(&self, lanes: usize) -> ([[*mut u8; STRIPS_PER_LANE]; MAX_LANES], usize) {
        let offset = self.ptr.align_offset(size_of::<Line>());
        let usable = self.len.saturating_sub(offset);
        let size = usable / (lanes * STRIPS_PER_LANE) / size_of::<Line>() * size_of::<Line>();
        let mut buffers = [[core::ptr::null_mut(); STRIPS_PER_LANE]; MAX_LANES];
        for (lane, pair) in buffers.iter_mut().enumerate().take(lanes) {
            for (i, buffer) in pair.iter_mut().enumerate() {
                *buffer = unsafe { self.ptr.add(offset + (lane * STRIPS_PER_LANE + i) * size) };
            }
        }
        (buffers, size)
    }
}

/// Strips of `area` row by row from the top, whole runs of `grid` rows each,
/// split across where `grid` rows of its width do not fit one buffer.
pub(crate) fn cut(area: Rect, format: ImageFormat, buffer_bytes: usize, grid: i16, out: &mut Vec<Rect, Caps>) -> Result<()> {
    if area.is_empty() {
        return Ok(());
    }
    let widest = (buffer_bytes / (format.row_bytes(1) * grid as usize)).min(i16::MAX as usize) as i16;
    if widest == 0 {
        return Err(Error::INVALID_SIZE);
    }
    let columns = (area.w + widest - 1) / widest;
    let w = (area.w + columns - 1) / columns;
    let rows = (buffer_bytes / format.row_bytes(w as usize)).min(i16::MAX as usize) as i16 / grid * grid;
    let mut y = area.y;
    while y < area.bottom() {
        let h = rows.min(area.bottom() - y);
        let mut x = area.x;
        while x < area.right() {
            let tile = w.min(area.right() - x);
            out.push(Rect::new(x, y, tile, h));
            x += tile;
        }
        y += h;
    }
    Ok(())
}

/// Puts display lists on a panel, drawing damaged rows into strips. What it
/// cannot do, it answers `None` or an error for, and the UI does without.
pub(crate) trait Presenter {
    /// The logical picture.
    fn area(&self) -> Rect;
    /// Damage and external rectangles are kept to whole runs of this many rows.
    fn row_grid(&self) -> i16;
    fn invalidate(&mut self, rect: Rect);
    fn invalidate_all(&mut self);
    /// Whether a frame is due without new damage: the first one, or one after
    /// a change of the display or the rotation.
    fn has_new_damage(&self) -> bool;
    /// Gets the next frame ready, waiting if the panel has no room for it yet.
    /// Read input and build the list after this, to show the freshest state.
    fn acquire(&mut self) -> Result<()>;
    /// What the next `render` repaints. May wait as `acquire` does.
    fn regions(&mut self) -> Result<&[Rect]>;
    fn render(&mut self, list: &DisplayList<'_, Caps>, cache: &mut GlyphCache<Caps>) -> Result<Stats>;
    /// Turns the picture from the next frame on, which repaints everything.
    fn set_rotation(&mut self, rotation: Rotation) -> Result<()>;
    /// Draws in `strips` from the next frame on, with `lanes` lanes, and returns
    /// the memory used so far. Refused, handing `strips` back, when they do not
    /// fit.
    fn set_strips(&mut self, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips>;
    /// Whether `config` asks the display for something it is not.
    fn changes_display(&self, config: &display::Config) -> bool;
    /// Whether `set_display` would take `strips` for `config`.
    fn fits_display(&self, config: &display::Config, strips: &Strips, lanes: usize) -> bool;
    /// `Display::reconfigure`, then `set_strips`, and everything is repainted.
    /// Nothing else may use the display meanwhile.
    unsafe fn set_display(&mut self, config: &display::Config, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips>;
    /// Leaves `hole` undrawn but for `over`, the UI written over what a
    /// surface wrote. With `hold`, frames go to the back framebuffer and the
    /// surface presents them; otherwise a hole draws in place. `true` when the
    /// next frame goes elsewhere than `regions` said.
    fn set_external(&mut self, hole: Option<Rect>, hold: bool, over: Option<Rect>) -> bool;
    /// While a surface presents: whether the framebuffer it takes next lacks
    /// something the ones before it were given.
    fn behind(&mut self) -> bool;
    /// What a surface needs to share the framebuffers with this presenter.
    fn buffers(&self) -> Buffers;
    /// The framebuffer with the last frame.
    fn shown(&self) -> Option<FrameBuffer>;
    /// The 2D engine copies may go to.
    fn blitter(&self) -> Option<NewBlitter>;
    /// Whether zoom and presence pictures are drawn at half size, for less
    /// time than their full resolution takes.
    fn half_pictures(&self) -> bool;
}
