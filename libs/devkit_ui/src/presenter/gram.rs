/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::CStr;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use bsp_rs::display::{self, Display, FrameBuffer, ImageFormat, Rotation, Ticket};
use devkit_gfx::{Bgr888, ClipStore, Damage, DisplayList, GlyphCache, Pixel, Prepared, Rect, Rgb565, Surface, draw, swap_rgb565};
use idf::heap::Caps;
use idf::log::{error, warn};
use idf::sync::Semaphore;
use idf::time::Instant;
use idf::{Error, Result, thread};

use super::{Config, MAX_LANES, Presenter, STRIPS_PER_LANE, Stats, StripSource, Strips, cut};
use crate::blit::NewBlitter;
use crate::external::{Buffers, NO_BACK};

const TAG: &CStr = c"presenter";

const MAX_REGIONS: usize = 8;
const LANE_STACK: usize = 8192;

struct Job {
    list: *const DisplayList<'static, Caps>,
    strips: Vec<Rect, Caps>,
    format: ImageFormat,
    buffers: [[*mut u8; STRIPS_PER_LANE]; MAX_LANES],
    /// The writes still reading each buffer.
    sent: [[Option<Ticket>; STRIPS_PER_LANE]; MAX_LANES],
}

struct Shared {
    display: &'static Display,
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
            if let Err(err) = self.display.wait(ticket) {
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
                if let Err(err) = shared.display.wait(ticket) {
                    error!(TAG, "write: {err}");
                }
                self.stall += t.elapsed().as_micros() as u32;
            }
            let t = Instant::now();
            let buffer = job.buffers[self.id][turn];
            let bytes = strip.w as usize * strip.h as usize * P::BYTES;
            let surface = unsafe { Surface::new(buffer, strip.w as usize * P::BYTES, strip) };
            draw::<P, _, _, _>(list, prepared, &mut self.clips, &surface);
            let pixels = unsafe { core::slice::from_raw_parts_mut(buffer, bytes) };
            if job.format == ImageFormat::Rgb565Be {
                swap_rgb565(pixels);
            }
            self.raster += t.elapsed().as_micros() as u32;
            match unsafe { shared.display.write(strip, pixels) } {
                Ok(ticket) => job.sent[self.id][turn] = Some(ticket),
                Err(err) => error!(TAG, "write {strip:?}: {err}"),
            }
        }
    }
}

/// Puts display lists on a panel that keeps the picture in its own memory and
/// turns it itself: damaged rows are drawn into strips and written while the
/// next ones are drawn.
pub(crate) struct GramPresenter {
    shared: Arc<Shared>,
    damage: Damage<MAX_REGIONS>,
    lanes: usize,
    strips: Strips,
    buffer_bytes: usize,
    hole: Option<Rect>,
    over: Option<Rect>,
    fresh: bool,
}

fn draws(format: ImageFormat) -> bool {
    matches!(format, ImageFormat::Rgb565 | ImageFormat::Rgb565Be | ImageFormat::Bgr888)
}

impl GramPresenter {
    pub(crate) fn new(display: &'static Display, config: Config) -> Result<Self> {
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
        let buffer_bytes = fit(&strips, config.lanes, format).ok_or(Error::INVALID_SIZE)?;
        let shared = Arc::new(Shared {
            display,
            start: [Semaphore::try_new(0, 1)?, Semaphore::try_new(0, 1)?],
            done: Semaphore::try_new(0, MAX_LANES as u32)?,
            next: AtomicUsize::new(0),
            raster_us: [AtomicU32::new(0), AtomicU32::new(0)],
            stall_us: [AtomicU32::new(0), AtomicU32::new(0)],
            job: UnsafeCell::new(Job {
                list: core::ptr::null(),
                strips: Vec::with_capacity_in(64, config.data_caps),
                format,
                buffers: [[core::ptr::null_mut(); STRIPS_PER_LANE]; MAX_LANES],
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
        let mut presenter = Self {
            shared,
            damage: Damage::new(Rect::default()),
            lanes: config.lanes,
            strips,
            buffer_bytes,
            hole: None,
            over: None,
            fresh: true,
        };
        presenter.set_rotation(config.rotation)?;
        Ok(presenter)
    }

    fn display(&self) -> &'static Display {
        self.shared.display
    }

    fn reset(&mut self) {
        self.damage = Damage::new(self.area());
        self.damage.add_full();
        self.fresh = true;
    }
}

/// Bytes per buffer of `strips` split for `lanes`, when one holds a row.
fn fit(strips: &Strips, lanes: usize, format: ImageFormat) -> Option<usize> {
    if !(1..=MAX_LANES).contains(&lanes) {
        return None;
    }
    let (_, buffer_bytes) = strips.buffers(lanes);
    if buffer_bytes < format.row_bytes(1) {
        error!(TAG, "strips of {buffer_bytes} bytes hold no pixel of {format:?}");
        return None;
    }
    Some(buffer_bytes)
}

impl Presenter for GramPresenter {
    fn area(&self) -> Rect {
        let size = self.display().size();
        Rect::new(0, 0, size.width as i16, size.height as i16)
    }

    fn row_grid(&self) -> i16 {
        1
    }

    fn invalidate(&mut self, rect: Rect) {
        self.damage.add(rect);
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
        job.buffers = self.strips.buffers(self.lanes).0;
        job.strips.clear();
        let hole = self.hole.unwrap_or_default();
        let over = self.over.unwrap_or_default();
        for region in self.damage.regions() {
            for area in region.subtract(&hole).chain([region.intersect(&over)]) {
                cut(area, job.format, self.buffer_bytes, 1, &mut job.strips)?;
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
        self.shared.drain();
        let mut config = display::Config::default();
        config.rotation = Some(rotation);
        let result = unsafe { self.display().reconfigure(&config) };
        self.reset();
        result
    }

    fn set_strips(&mut self, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        let Some(buffer_bytes) = fit(&strips, lanes, self.display().pixel_format()) else {
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
        fit(strips, lanes, format).is_some()
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
