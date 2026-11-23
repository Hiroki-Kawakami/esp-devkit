/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Draws display lists into strips, a lane per core, and writes them into a
//! panel's own memory, turned and in its format, while the next are drawn.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::CStr;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use bsp_rs::display::{Display, Gram, ImageFormat, Rotation, Ticket};
use devkit_gfx::{
    Bgr888, ClipStore, DisplayList, GlyphCache, Image, L8, Pixel, Prepared, Rect, Rgb565, Size, Surface, Target, copy, draw, pack_l1, pack_l4,
    swap_rgb565,
};
use idf::heap::Caps;
use idf::log::{error, warn};
use idf::sync::Semaphore;
use idf::time::Instant;
use idf::{Error, Result, thread};

use super::{Config, Grid, MAX_LANES, STRIPS_PER_LANE, Stats, StripSource, Strips, cut, panel_rect, split_turn};

const TAG: &CStr = c"presenter";

const LANE_STACK: usize = 8192;

/// What strips are drawn in for a panel taking `format`.
pub(crate) fn drawn_format(format: ImageFormat) -> Option<ImageFormat> {
    match format {
        ImageFormat::Rgb565 | ImageFormat::Rgb565Be => Some(ImageFormat::Rgb565),
        ImageFormat::Bgr888 => Some(ImageFormat::Bgr888),
        ImageFormat::L8 | ImageFormat::L4 | ImageFormat::L1 => Some(ImageFormat::L8),
        _ => None,
    }
}

/// Puts `pixels`, rows of `width` as drawn, in the panel's `format`, and
/// returns the bytes they take then.
fn finish(format: ImageFormat, pixels: &mut [u8], width: usize) -> usize {
    match format {
        ImageFormat::Rgb565Be => {
            swap_rgb565(pixels);
            pixels.len()
        }
        ImageFormat::L4 => pack_l4(pixels, width),
        ImageFormat::L1 => pack_l1(pixels, width),
        _ => pixels.len(),
    }
}

struct Job {
    list: *const DisplayList<'static, Caps>,
    strips: Vec<Rect, Caps>,
    /// The panel's.
    format: ImageFormat,
    drawn: ImageFormat,
    /// What is left after the panel's own turn, done here.
    turn: Rotation,
    /// The panel's picture, before `turn`.
    panel: Size,
    buffers: [[*mut u8; STRIPS_PER_LANE]; MAX_LANES],
    /// Where a lane draws a strip it then turns into `buffers`.
    scratch: [*mut u8; MAX_LANES],
    /// The writes still reading each buffer.
    sent: [[Option<Ticket>; STRIPS_PER_LANE]; MAX_LANES],
}

struct Shared {
    gram: Gram<'static>,
    start: [Semaphore; MAX_LANES],
    done: Semaphore,
    next: AtomicUsize,
    raster_us: [AtomicU32; MAX_LANES],
    stall_us: [AtomicU32; MAX_LANES],
    job: UnsafeCell<Job>,
    prepared: UnsafeCell<Prepared<Caps>>,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl Shared {
    /// Between frames only.
    fn drain(&self) {
        let job = unsafe { &mut *self.job.get() };
        for ticket in job.sent.iter_mut().flatten().filter_map(Option::take) {
            if let Err(err) = self.gram.wait(ticket) {
                error!(TAG, "write: {err}");
            }
        }
    }
}

struct Lane {
    id: usize,
    turn: usize,
    raster: u32,
    stall: u32,
    clips: ClipStore<Caps>,
}

impl Lane {
    fn run(mut self, shared: Arc<Shared>) {
        loop {
            shared.start[self.id].take();
            // `StripWriter::render` keeps the job and the list alive until `done`.
            let job = unsafe { &mut *shared.job.get() };
            let prepared = unsafe { &*shared.prepared.get() };
            let list = unsafe { &*job.list };
            (self.raster, self.stall) = (0, 0);
            match job.drawn {
                ImageFormat::Bgr888 => self.strips::<Bgr888>(&shared, job, list, prepared),
                ImageFormat::L8 => self.strips::<L8>(&shared, job, list, prepared),
                _ => self.strips::<Rgb565>(&shared, job, list, prepared),
            }
            shared.raster_us[self.id].store(self.raster, Ordering::Relaxed);
            shared.stall_us[self.id].store(self.stall, Ordering::Relaxed);
            shared.done.give();
        }
    }

    fn strips<P: Pixel>(&mut self, shared: &Shared, job: &mut Job, list: &DisplayList<'static, Caps>, prepared: &Prepared<Caps>) {
        loop {
            let index = shared.next.fetch_add(1, Ordering::Relaxed);
            let Some(&strip) = job.strips.get(index) else { return };
            let turn = self.turn;
            self.turn = (self.turn + 1) % STRIPS_PER_LANE;
            if let Some(ticket) = job.sent[self.id][turn].take() {
                let t = Instant::now();
                if let Err(err) = shared.gram.wait(ticket) {
                    error!(TAG, "write: {err}");
                }
                self.stall += t.elapsed().as_micros() as u32;
            }
            let t = Instant::now();
            let buffer = job.buffers[self.id][turn];
            let drawn = if job.turn == Rotation::R0 { buffer } else { job.scratch[self.id] };
            let bytes = strip.w as usize * strip.h as usize * P::BYTES;
            let surface = unsafe { Surface::new(drawn, strip.w as usize * P::BYTES, strip) };
            draw::<P, _, _, _>(list, prepared, &mut self.clips, &surface);
            let pixels = unsafe { core::slice::from_raw_parts_mut(buffer, bytes) };
            let area = if job.turn == Rotation::R0 {
                Some(strip)
            } else {
                let drawn = unsafe { core::slice::from_raw_parts_mut(drawn, bytes) };
                turn_strip(job, strip, drawn, pixels)
            };
            let Some(area) = area else {
                self.raster += t.elapsed().as_micros() as u32;
                error!(TAG, "turn {strip:?}: outside the panel");
                continue;
            };
            let len = finish(job.format, pixels, area.w as usize);
            self.raster += t.elapsed().as_micros() as u32;
            match unsafe { shared.gram.write(area, &pixels[..len]) } {
                Ok(ticket) => job.sent[self.id][turn] = Some(ticket),
                Err(err) => error!(TAG, "write {area:?}: {err}"),
            }
        }
    }
}

/// Copies `drawn`, `strip` of the logical picture, into `out` as the panel
/// holds it, and says where on the panel that lies.
fn turn_strip(job: &Job, strip: Rect, drawn: &mut [u8], out: &mut [u8]) -> Option<Rect> {
    let area = panel_rect(strip, job.turn, job.panel)?;
    let image = Image::from_pixels(job.drawn, strip.w as u16, strip.h as u16, strip.w as usize, drawn);
    let mut target = Target::new(out, job.drawn, Size::new(area.w as u16, area.h as u16), area.w as usize)?.rotated(job.turn);
    copy(&image, &mut target).then_some(area)
}

/// Bytes per buffer of `strips` split for `lanes`, with room to draw before
/// turning when `turns`, when one holds a row drawn for a panel in `format`.
fn fit(strips: &Strips, lanes: usize, format: ImageFormat, turns: bool) -> Option<usize> {
    if !(1..=MAX_LANES).contains(&lanes) {
        return None;
    }
    let drawn = drawn_format(format)?;
    let buffer_bytes = if turns { strips.buffers_with_scratch(lanes).2 } else { strips.buffers(lanes).1 };
    if buffer_bytes < drawn.row_bytes(1) {
        error!(TAG, "strips of {buffer_bytes} bytes hold no pixel of {drawn:?}");
        return None;
    }
    Some(buffer_bytes)
}

/// Writes display lists into a panel's own memory. A turn the panel cannot
/// take is done on each strip before it goes.
pub(crate) struct StripWriter {
    shared: Arc<Shared>,
    grid: Grid,
    lanes: usize,
    strips: Strips,
    buffer_bytes: usize,
    turn: Rotation,
}

impl StripWriter {
    /// Turned as `config.rotation` says.
    pub(crate) fn new(gram: Gram<'static>, config: Config) -> Result<Self> {
        let display = gram.display();
        if !(1..=MAX_LANES).contains(&config.lanes) {
            return Err(Error::INVALID_ARG);
        }
        let format = display.pixel_format();
        let Some(drawn) = drawn_format(format) else {
            error!(TAG, "no strips for {format:?}");
            return Err(Error::NOT_SUPPORTED);
        };
        let strips = match config.strips {
            StripSource::Allocate { bytes, caps } => Strips::allocate(bytes, caps)?,
            StripSource::Given(strips) => strips,
        };
        let buffer_bytes = fit(&strips, config.lanes, format, false).ok_or(Error::INVALID_SIZE)?;
        let shared = Arc::new(Shared {
            gram,
            start: [Semaphore::try_new(0, 1)?, Semaphore::try_new(0, 1)?],
            done: Semaphore::try_new(0, MAX_LANES as u32)?,
            next: AtomicUsize::new(0),
            raster_us: [AtomicU32::new(0), AtomicU32::new(0)],
            stall_us: [AtomicU32::new(0), AtomicU32::new(0)],
            job: UnsafeCell::new(Job {
                list: core::ptr::null(),
                strips: Vec::with_capacity_in(64, config.data_caps),
                format,
                drawn,
                turn: Rotation::R0,
                panel: display.size(),
                buffers: [[core::ptr::null_mut(); STRIPS_PER_LANE]; MAX_LANES],
                scratch: [core::ptr::null_mut(); MAX_LANES],
                sent: [[None; STRIPS_PER_LANE]; MAX_LANES],
            }),
            prepared: UnsafeCell::new(Prepared::new_in(config.data_caps)),
        });
        for id in 0..MAX_LANES {
            let lane = Lane { id, turn: 0, raster: 0, stall: 0, clips: ClipStore::new_in(crate::INTERNAL_RAM) };
            let shared = shared.clone();
            let builder = thread::Builder::new()
                .name("ui_lane")
                .stack_size(LANE_STACK)
                .priority(config.priority)
                .core(Some(config.lane_cores[id]))
                .stack_caps(config.lane_stack_caps);
            builder.spawn(move || lane.run(shared))?;
        }
        let align = display.alignment() as i16;
        let mut writer = Self { shared, grid: Grid { columns: align, rows: align }, lanes: config.lanes, strips, buffer_bytes, turn: Rotation::R0 };
        writer.set_rotation(config.rotation)?;
        Ok(writer)
    }

    pub(crate) fn display(&self) -> &'static Display {
        self.shared.gram.display()
    }

    /// What strips keep to: the panel's alignment.
    pub(crate) fn grid(&self) -> Grid {
        self.grid
    }

    /// The logical picture.
    pub(crate) fn area(&self) -> Rect {
        let size = self.display().size();
        let (w, h) = (size.width as i16, size.height as i16);
        if self.turn.swaps_axes() { Rect::new(0, 0, h, w) } else { Rect::new(0, 0, w, h) }
    }

    /// Where `rect` of the logical picture lies on the panel.
    pub(crate) fn panel_rect(&self, rect: Rect) -> Option<Rect> {
        panel_rect(rect, self.turn, self.display().size())
    }

    /// Draws and writes `regions`, which lie on the grid, but for `hole` and
    /// with `over`, and returns once every strip is written.
    pub(crate) fn render(&mut self, list: &DisplayList<'_, Caps>, cache: &mut GlyphCache<Caps>, regions: &[Rect], hole: Rect, over: Rect) -> Result<Stats> {
        let mut stats = Stats::default();
        let shared = &*self.shared;
        let t0 = Instant::now();
        let prepared = unsafe { &mut *shared.prepared.get() };
        let decoded = cache.decoded();
        prepared.prepare(list, regions, cache);
        stats.glyphs_decoded = cache.decoded() - decoded;
        stats.glyphs_missing = prepared.missing();
        if list.skipped() > 0 {
            warn!(TAG, "{} images in a format draw cannot take were left out", list.skipped());
        }
        let t1 = Instant::now();

        let job = unsafe { &mut *shared.job.get() };
        job.list = core::ptr::from_ref(list).cast();
        (job.turn, job.panel) = (self.turn, self.display().size());
        if self.turn == Rotation::R0 {
            job.buffers = self.strips.buffers(self.lanes).0;
        } else {
            (job.buffers, job.scratch, _) = self.strips.buffers_with_scratch(self.lanes);
        }
        job.strips.clear();
        for region in regions {
            for area in region.subtract(&hole).chain([region.intersect(&over)]) {
                cut(area, job.drawn, self.buffer_bytes, self.grid, &mut job.strips)?;
            }
        }
        stats.strips = job.strips.len() as u32;
        shared.next.store(0, Ordering::Relaxed);
        // A lane on this task's core preempts it, so that lane starts last.
        for start in shared.start[..self.lanes].iter().rev() {
            start.give();
        }
        for _ in 0..self.lanes {
            shared.done.take();
        }
        for lane in 0..self.lanes {
            stats.raster_us += shared.raster_us[lane].load(Ordering::Relaxed);
            stats.stall_us += shared.stall_us[lane].load(Ordering::Relaxed);
        }
        stats.prepare_us = t1.duration_since(t0).as_micros() as u32;
        stats.draw_us = t1.elapsed().as_micros() as u32;
        Ok(stats)
    }

    /// Everything is to be drawn again after, whatever it returns.
    pub(crate) fn set_rotation(&mut self, rotation: Rotation) -> Result<()> {
        let display = self.display();
        let Some((panel, turn)) = split_turn(rotation, display.rotations()) else {
            error!(TAG, "the panel takes no rotation");
            return Err(Error::NOT_SUPPORTED);
        };
        let size = display.size();
        let align = self.grid.columns as u16;
        if turn.swaps_axes() && (size.width % align != 0 || size.height % align != 0) {
            error!(TAG, "a {}x{} panel turned here leaves its {align}-pixel grid", size.width, size.height);
            return Err(Error::INVALID_SIZE);
        }
        let buffer_bytes = fit(&self.strips, self.lanes, display.pixel_format(), turn != Rotation::R0).ok_or(Error::INVALID_SIZE)?;
        self.shared.drain();
        let result = unsafe { display.reconfigure(None, Some(panel)) };
        if result.is_ok() {
            (self.turn, self.buffer_bytes) = (turn, buffer_bytes);
        }
        result
    }

    pub(crate) fn set_strips(&mut self, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        let Some(buffer_bytes) = fit(&strips, lanes, self.display().pixel_format(), self.turn != Rotation::R0) else {
            return Err(strips);
        };
        self.shared.drain();
        self.lanes = lanes;
        self.buffer_bytes = buffer_bytes;
        Ok(core::mem::replace(&mut self.strips, strips))
    }

    /// Whether `strips` for `lanes` hold rows drawn for a panel in `format`.
    pub(crate) fn fits(&self, format: ImageFormat, strips: &Strips, lanes: usize) -> bool {
        fit(strips, lanes, format, self.turn != Rotation::R0).is_some()
    }
}
