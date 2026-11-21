/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::CStr;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use bsp_rs::display::{self, Display, FrameBuffer, Gram, ImageFormat, Rotation, Ticket};
use devkit_gfx::{
    Bgr888, ClipStore, Damage, DisplayList, GlyphCache, Image, Pixel, Prepared, Rect, Rgb565, Size, Surface, Target, copy, draw, swap_rgb565,
};
use idf::heap::Caps;
use idf::log::{error, warn};
use idf::sync::Semaphore;
use idf::time::Instant;
use idf::{Error, Result, thread};

use super::{Config, Grid, MAX_LANES, Presenter, STRIPS_PER_LANE, Stats, StripSource, Strips, cut, panel_rect, split_turn};
use crate::blit::NewBlitter;
use crate::external::{Buffers, NO_BACK};

const TAG: &CStr = c"presenter";

const MAX_REGIONS: usize = 8;
const LANE_STACK: usize = 8192;

struct Job {
    list: *const DisplayList<'static, Caps>,
    strips: Vec<Rect, Caps>,
    format: ImageFormat,
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
            // `GramPresenter::render` keeps the job and the list alive until `done`.
            let job = unsafe { &mut *shared.job.get() };
            let prepared = unsafe { &*shared.prepared.get() };
            let list = unsafe { &*job.list };
            (self.raster, self.stall) = (0, 0);
            match job.format {
                ImageFormat::Bgr888 => self.strips::<Bgr888>(&shared, job, list, prepared),
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
                if job.format == ImageFormat::Rgb565Be {
                    swap_rgb565(pixels);
                }
                Some(strip)
            } else {
                let drawn = unsafe { core::slice::from_raw_parts_mut(drawn, bytes) };
                turn_strip(job, strip, drawn, pixels)
            };
            self.raster += t.elapsed().as_micros() as u32;
            let Some(area) = area else {
                error!(TAG, "turn {strip:?}: outside the panel");
                continue;
            };
            match unsafe { shared.gram.write(area, pixels) } {
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
    // Swapping in place first is faster than `copy` swapping as it turns.
    if job.format == ImageFormat::Rgb565Be {
        swap_rgb565(drawn);
    }
    let image = Image::from_pixels(job.format, strip.w as u16, strip.h as u16, strip.w as usize, drawn);
    let mut target = Target::new(out, job.format, Size::new(area.w as u16, area.h as u16), area.w as usize)?.rotated(job.turn);
    copy(&image, &mut target).then_some(area)
}

/// Puts display lists on a panel that keeps the picture in its own memory:
/// damaged rows are drawn into strips and written while the next ones are
/// drawn. A turn the panel cannot take is done on each strip before it goes.
pub(crate) struct GramPresenter {
    shared: Arc<Shared>,
    grid: Grid,
    damage: Damage<MAX_REGIONS>,
    lanes: usize,
    strips: Strips,
    buffer_bytes: usize,
    turn: Rotation,
    hole: Option<Rect>,
    over: Option<Rect>,
    fresh: bool,
}

fn draws(format: ImageFormat) -> bool {
    matches!(format, ImageFormat::Rgb565 | ImageFormat::Rgb565Be | ImageFormat::Bgr888)
}

impl GramPresenter {
    pub(crate) fn new(gram: Gram<'static>, config: Config) -> Result<Self> {
        let display = gram.display();
        if !(1..=MAX_LANES).contains(&config.lanes) {
            return Err(Error::INVALID_ARG);
        }
        let format = display.pixel_format();
        if !draws(format) {
            error!(TAG, "no strips in {format:?}");
            return Err(Error::NOT_SUPPORTED);
        }
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
                .core(Some(id as u32))
                .stack_caps(config.lane_stack_caps);
            builder.spawn(move || lane.run(shared))?;
        }
        let align = display.alignment() as i16;
        let mut presenter = Self {
            shared,
            grid: Grid { columns: align, rows: align },
            damage: Damage::new(Rect::default()),
            lanes: config.lanes,
            strips,
            buffer_bytes,
            turn: Rotation::R0,
            hole: None,
            over: None,
            fresh: true,
        };
        presenter.set_rotation(config.rotation)?;
        Ok(presenter)
    }

    fn display(&self) -> &'static Display {
        self.shared.gram.display()
    }

    fn reset(&mut self) {
        self.damage = Damage::new(self.area());
        self.damage.add_full();
        self.fresh = true;
    }
}

/// Bytes per buffer of `strips` split for `lanes`, with room to draw before
/// turning when `turns`, when one holds a row.
fn fit(strips: &Strips, lanes: usize, format: ImageFormat, turns: bool) -> Option<usize> {
    if !(1..=MAX_LANES).contains(&lanes) {
        return None;
    }
    let buffer_bytes = if turns { strips.buffers_with_scratch(lanes).2 } else { strips.buffers(lanes).1 };
    if buffer_bytes < format.row_bytes(1) {
        error!(TAG, "strips of {buffer_bytes} bytes hold no pixel of {format:?}");
        return None;
    }
    Some(buffer_bytes)
}

impl Presenter for GramPresenter {
    fn area(&self) -> Rect {
        let size = self.display().size();
        let (w, h) = (size.width as i16, size.height as i16);
        if self.turn.swaps_axes() { Rect::new(0, 0, h, w) } else { Rect::new(0, 0, w, h) }
    }

    fn grid(&self) -> Grid {
        self.grid
    }

    fn invalidate(&mut self, rect: Rect) {
        self.damage.add(self.grid.around(rect));
    }

    fn invalidate_all(&mut self) {
        self.damage.add_full();
    }

    fn has_new_damage(&self) -> bool {
        self.fresh
    }

    fn acquire(&mut self) -> Result<()> {
        Ok(())
    }

    fn regions(&mut self) -> Result<&[Rect]> {
        Ok(self.damage.regions())
    }

    fn render(&mut self, list: &DisplayList<'_, Caps>, cache: &mut GlyphCache<Caps>) -> Result<Stats> {
        self.fresh = false;
        let mut stats = Stats::default();
        if self.damage.is_empty() {
            return Ok(stats);
        }
        let shared = &*self.shared;
        let t0 = Instant::now();
        let prepared = unsafe { &mut *shared.prepared.get() };
        let decoded = cache.decoded();
        prepared.prepare(list, self.damage.regions(), cache);
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
        let hole = self.hole.unwrap_or_default();
        let over = self.over.unwrap_or_default();
        for region in self.damage.regions() {
            for area in region.subtract(&hole).chain([region.intersect(&over)]) {
                cut(area, job.format, self.buffer_bytes, self.grid, &mut job.strips)?;
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
        self.damage.clear();
        for lane in 0..self.lanes {
            stats.raster_us += shared.raster_us[lane].load(Ordering::Relaxed);
            stats.stall_us += shared.stall_us[lane].load(Ordering::Relaxed);
        }
        stats.prepare_us = t1.duration_since(t0).as_micros() as u32;
        stats.draw_us = t1.elapsed().as_micros() as u32;
        Ok(stats)
    }

    fn set_rotation(&mut self, rotation: Rotation) -> Result<()> {
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
        self.reset();
        result
    }

    fn set_strips(&mut self, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        let Some(buffer_bytes) = fit(&strips, lanes, self.display().pixel_format(), self.turn != Rotation::R0) else {
            return Err(strips);
        };
        self.shared.drain();
        self.lanes = lanes;
        self.buffer_bytes = buffer_bytes;
        Ok(core::mem::replace(&mut self.strips, strips))
    }

    fn changes_display(&self, config: &display::Config) -> bool {
        config.pixel_format.is_some_and(|f| f != self.display().pixel_format()) || config.fb_num != 0
    }

    fn fits_display(&self, config: &display::Config, strips: &Strips, lanes: usize) -> bool {
        let format = config.pixel_format.unwrap_or_else(|| self.display().pixel_format());
        fit(strips, lanes, format, self.turn != Rotation::R0).is_some()
    }

    unsafe fn set_display(&mut self, config: &display::Config, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        if self.changes_display(config) {
            error!(TAG, "the panel keeps its format and has no framebuffers");
            return Err(strips);
        }
        self.set_strips(strips, lanes)
    }

    fn set_external(&mut self, hole: Option<Rect>, _hold: bool, over: Option<Rect>) -> bool {
        (self.hole, self.over) = (hole, over);
        false
    }

    fn behind(&mut self) -> bool {
        false
    }

    fn buffers(&self) -> Buffers {
        Buffers { latest: Arc::new(AtomicUsize::new(0)), back: Arc::new(AtomicUsize::new(NO_BACK)), count: 0 }
    }

    fn shown(&self) -> Option<FrameBuffer> {
        None
    }

    fn blitter(&self) -> Option<NewBlitter> {
        None
    }

    fn half_pictures(&self) -> bool {
        false
    }
}
