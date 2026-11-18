/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::alloc::Allocator;
use core::cell::UnsafeCell;
use core::ffi::CStr;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use core::time::Duration;

use bsp_rs::display::{self, Display, FrameBuffer, ImageFormat, Rotation};
use devkit_gfx::{Bgr888, ClipStore, Damage, DisplayList, GlyphCache, Pixel, Prepared, Rect, Rgb565, Size, Surface, draw};
use idf::heap::Caps;
use idf::log::{error, warn};
use idf::sync::Semaphore;
use idf::time::Instant;
use idf::{Error, Result, thread};

use super::ppa::PpaBlitter;
use super::{Config, MAX_FRAME_BUFFERS, MAX_LANES, PresentMode, Presenter, STRIPS_PER_LANE, Stats, StripSource, Strips, cut};
use crate::blit::{self, Blit, Blitter, NewBlitter};
use crate::external::{Buffers, NO_BACK};

const TAG: &CStr = c"presenter";

const MAX_REGIONS: usize = 8;
const LANE_STACK: usize = 8192;
const STALL_TIMEOUT: Duration = Duration::from_secs(1);
// The 2D engine's cost steps every 16 rows, and it hangs on short rotated
// copies, so damage and strips stay on this grid.
const ROW_GRID: i16 = 16;

struct Job {
    list: *const DisplayList<'static, Caps>,
    strips: Vec<Rect, Caps>,
    fb: Option<FrameBuffer>,
    rotation: Rotation,
    buffers: [[*mut u8; STRIPS_PER_LANE]; MAX_LANES],
}

struct Shared {
    start: [Semaphore; MAX_LANES],
    done: Semaphore,
    next: AtomicUsize,
    frame: AtomicU32,
    raster_us: [AtomicU32; MAX_LANES],
    stall_us: [AtomicU32; MAX_LANES],
    job: UnsafeCell<Job>,
    prepared: UnsafeCell<Prepared<Caps>>,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

struct Lane {
    id: usize,
    blits: PpaBlitter,
    turn: usize,
    last: Rect,
    before: Rect,
    raster: u32,
    stall: u32,
    clips: ClipStore<Caps>,
}

impl Lane {
    fn run(mut self, shared: Arc<Shared>) {
        loop {
            shared.start[self.id].take();
            // `FbPresenter::render` keeps the job and the list alive until `done`.
            let job = unsafe { &*shared.job.get() };
            let prepared = unsafe { &*shared.prepared.get() };
            let list = unsafe { &*job.list };
            (self.raster, self.stall) = (0, 0);
            match job.fb.map(|fb| fb.format()) {
                Some(ImageFormat::Bgr888) => self.strips::<Bgr888>(&shared, job, list, prepared),
                _ => self.strips::<Rgb565>(&shared, job, list, prepared),
            }
            while self.blits.pending() > 0 {
                self.wait_blit(&shared, job);
            }
            shared.raster_us[self.id].store(self.raster, Ordering::Relaxed);
            shared.stall_us[self.id].store(self.stall, Ordering::Relaxed);
            shared.done.give();
        }
    }

    fn wait_blit(&mut self, shared: &Shared, job: &Job) {
        let t = Instant::now();
        loop {
            match self.blits.wait_timeout(STALL_TIMEOUT) {
                Some(Ok(())) => break,
                Some(Err(err)) => {
                    error!(TAG, "copy: {err}");
                    break;
                }
                None => error!(
                    TAG,
                    "lane {}: blit stalled, last {:?}, before {:?}, pending {}, strip {}/{}, frame {}, {:?}",
                    self.id,
                    self.last,
                    self.before,
                    self.blits.pending(),
                    shared.next.load(Ordering::Relaxed),
                    job.strips.len(),
                    shared.frame.load(Ordering::Relaxed),
                    job.rotation,
                ),
            }
        }
        self.stall += t.elapsed().as_micros() as u32;
    }

    fn strips<P: Pixel>(&mut self, shared: &Shared, job: &Job, list: &DisplayList<'static, Caps>, prepared: &Prepared<Caps>) {
        let Some(fb) = job.fb else { return };
        loop {
            let index = shared.next.fetch_add(1, Ordering::Relaxed);
            let Some(strip) = job.strips.get(index) else { return };
            if self.blits.pending() == STRIPS_PER_LANE {
                self.wait_blit(shared, job);
            }
            let t = Instant::now();
            let buffer = job.buffers[self.id][self.turn];
            self.turn = (self.turn + 1) % STRIPS_PER_LANE;
            self.before = self.last;
            self.last = *strip;
            let surface = unsafe { Surface::new(buffer, strip.w as usize * P::BYTES, *strip) };
            draw::<P, _, _, _>(list, prepared, &mut self.clips, &surface);
            self.raster += t.elapsed().as_micros() as u32;
            let Some(target) = (unsafe { fb.target() }).and_then(|t| t.rotated(job.rotation).crop(*strip)) else {
                error!(TAG, "blit {strip:?}: outside the panel");
                continue;
            };
            let size = Size::new(strip.w as u16, strip.h as u16);
            let mut op = Blit::new(unsafe { blit::image(buffer, fb.format(), size) });
            op.at = (strip.x, strip.y);
            if let Err(err) = self.blits.blit(target, &op) {
                error!(TAG, "blit {strip:?}: {err}");
            }
        }
    }
}

/// Puts display lists on a panel with framebuffers: damaged rows are drawn
/// into internal RAM strips and copied, rotated as needed, by the PPA.
pub struct FbPresenter {
    display: &'static Display,
    shared: Arc<Shared>,
    damage: [Damage<MAX_REGIONS>; MAX_FRAME_BUFFERS],
    mode: PresentMode,
    frame_buffers: usize,
    rotation: Rotation,
    lanes: usize,
    strips: Strips,
    buffer_bytes: usize,
    buffers: Buffers,
    hole: Option<Rect>,
    hold: bool,
    over: Option<Rect>,
    target: Option<usize>,
    fresh: bool,
}

fn area(display: &Display, rotation: Rotation) -> Rect {
    let size = display.size();
    let (w, h) = (size.width as i16, size.height as i16);
    match rotation {
        Rotation::R0 | Rotation::R180 => Rect::new(0, 0, w, h),
        Rotation::R90 | Rotation::R270 => Rect::new(0, 0, h, w),
    }
}

fn holds_block(format: ImageFormat, buffer_bytes: usize) -> bool {
    if buffer_bytes < format.row_bytes(ROW_GRID as usize) * ROW_GRID as usize {
        error!(TAG, "strips of {buffer_bytes} bytes hold no 16x16 block of {format:?}");
        return false;
    }
    true
}

impl FbPresenter {
    /// Refused when a strip buffer cannot hold a 16x16 block.
    pub fn new(display: &'static Display, config: Config) -> Result<Self> {
        if !(1..=MAX_LANES).contains(&config.lanes) {
            return Err(Error::INVALID_ARG);
        }
        let strips = match config.strips {
            StripSource::Allocate { bytes, caps } => Strips::allocate(bytes, caps)?,
            StripSource::Given(strips) => strips,
        };
        let (_, buffer_bytes) = strips.buffers(config.lanes);
        if !holds_block(display.pixel_format(), buffer_bytes) {
            return Err(Error::INVALID_SIZE);
        }
        let shared = Arc::new(Shared {
            start: [Semaphore::try_new(0, 1)?, Semaphore::try_new(0, 1)?],
            done: Semaphore::try_new(0, MAX_LANES as u32)?,
            next: AtomicUsize::new(0),
            frame: AtomicU32::new(0),
            raster_us: [AtomicU32::new(0), AtomicU32::new(0)],
            stall_us: [AtomicU32::new(0), AtomicU32::new(0)],
            job: UnsafeCell::new(Job {
                list: core::ptr::null(),
                strips: Vec::with_capacity_in(64, config.data_caps),
                fb: None,
                rotation: Rotation::R0,
                buffers: [[core::ptr::null_mut(); STRIPS_PER_LANE]; MAX_LANES],
            }),
            prepared: UnsafeCell::new(Prepared::new_in(config.data_caps)),
        });
        for id in 0..MAX_LANES {
            let lane = Lane {
                id,
                blits: PpaBlitter::new(STRIPS_PER_LANE)?,
                turn: 0,
                last: Rect::default(),
                before: Rect::default(),
                raster: 0,
                stall: 0,
                clips: ClipStore::new_in(Caps::INTERNAL),
            };
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
            display,
            shared,
            damage: [Damage::new(Rect::default()); MAX_FRAME_BUFFERS],
            mode: config.mode,
            frame_buffers: config.frame_buffers,
            rotation: config.rotation,
            lanes: config.lanes,
            strips,
            buffer_bytes,
            buffers: Buffers {
                latest: Arc::new(AtomicUsize::new(display.scanning().unwrap_or(0))),
                back: Arc::new(AtomicUsize::new(NO_BACK)),
                count: 1,
            },
            hole: None,
            hold: false,
            over: None,
            target: None,
            fresh: true,
        };
        presenter.reset()?;
        Ok(presenter)
    }

    fn reset(&mut self) -> Result<()> {
        let area = self.area();
        self.damage = [Damage::new(area); MAX_FRAME_BUFFERS];
        self.invalidate_all();
        self.target = None;
        self.fresh = true;
        if self.mode == PresentMode::InPlace {
            let latest = self.latest() % self.frame_buffers();
            self.buffers.latest.store(latest, Ordering::Release);
            self.display.present(latest)?;
        }
        Ok(())
    }

    fn latest(&self) -> usize {
        self.buffers.latest.load(Ordering::Acquire)
    }

    fn in_place(&self) -> bool {
        (self.mode == PresentMode::InPlace || self.hole.is_some()) && !self.hold
    }

    fn frame_buffers(&self) -> usize {
        self.frame_buffers.min(self.display.frame_buffer_count()).max(1)
    }

    pub fn configure(&mut self, mode: PresentMode, frame_buffers: usize, rotation: Rotation, lanes: usize) -> Result<()> {
        if !(1..=MAX_LANES).contains(&lanes) {
            return Err(Error::INVALID_ARG);
        }
        (self.mode, self.frame_buffers, self.rotation, self.lanes) = (mode, frame_buffers, rotation, lanes);
        self.reset()
    }

    /// Draws in `strips` from the next frame on, with `lanes` lanes, and returns
    /// the memory used so far: nothing draws in it any more. Refused, handing
    /// `strips` back, when a buffer cannot hold a 16x16 block.
    pub fn set_strips(&mut self, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        let Some(buffer_bytes) = self.fit(&strips, lanes, self.display.pixel_format()) else {
            return Err(strips);
        };
        self.lanes = lanes;
        self.buffer_bytes = buffer_bytes;
        Ok(core::mem::replace(&mut self.strips, strips))
    }

    fn fit(&self, strips: &Strips, lanes: usize, format: ImageFormat) -> Option<usize> {
        if !(1..=MAX_LANES).contains(&lanes) {
            return None;
        }
        let (_, buffer_bytes) = strips.buffers(lanes);
        holds_block(format, buffer_bytes).then_some(buffer_bytes)
    }

    fn format_after(&self, config: &display::Config) -> ImageFormat {
        config.pixel_format.unwrap_or_else(|| self.display.pixel_format())
    }

    /// The logical picture.
    pub fn area(&self) -> Rect {
        area(self.display, self.rotation)
    }

    pub fn invalidate(&mut self, rect: Rect) {
        let top = rect.y.div_euclid(ROW_GRID) * ROW_GRID;
        let bottom = (rect.bottom() + ROW_GRID - 1).div_euclid(ROW_GRID) * ROW_GRID;
        let aligned = Rect::new(rect.x, top, rect.w, bottom - top);
        for damage in &mut self.damage {
            damage.add(aligned);
        }
    }

    pub fn invalidate_all(&mut self) {
        for damage in &mut self.damage {
            damage.add_full();
        }
    }

    fn wait_flip(&self) -> Result<()> {
        while self.display.scanning().is_some_and(|i| i != self.latest()) {
            self.display.wait_frame()?;
        }
        Ok(())
    }

    /// Picks the framebuffer of the next frame, waiting if none is free. Read
    /// input and build the list after this, to show the freshest state.
    pub fn acquire(&mut self) -> Result<()> {
        if self.target.is_some() {
            return Ok(());
        }
        let count = self.frame_buffers();
        let target = if self.in_place() || count < 2 {
            self.latest()
        } else if self.hold {
            self.buffers().claim_back(self.display)
        } else if count == 2 {
            self.wait_flip()?;
            (self.latest() + 1) % count
        } else {
            let latest = self.latest();
            let scanning = self.display.scanning().unwrap_or(latest);
            (0..count).find(|&i| i != scanning && i != latest).unwrap_or((latest + 1) % count)
        };
        self.target = Some(target);
        Ok(())
    }

    /// Draws what was invalidated and shows it. `Swap` returns once the frame
    /// is queued for the next boundary.
    pub fn render<C: Allocator + Clone>(&mut self, list: &DisplayList<'_, Caps>, cache: &mut GlyphCache<C>) -> Result<Stats> {
        self.acquire()?;
        self.fresh = false;
        let target = self.target.take().ok_or(Error::INVALID_STATE)?;
        let mut stats = Stats::default();
        if self.damage[target].is_empty() {
            return Ok(stats);
        }
        let fb = self.display.frame_buffer(target).ok_or(Error::INVALID_STATE)?;
        let shared = &*self.shared;
        let t0 = Instant::now();
        let prepared = unsafe { &mut *shared.prepared.get() };
        let decoded = cache.decoded();
        prepared.prepare(list, self.damage[target].regions(), cache);
        stats.glyphs_decoded = cache.decoded() - decoded;
        stats.glyphs_missing = prepared.missing();
        if list.skipped() > 0 {
            warn!(TAG, "{} images in a format draw cannot take were left out", list.skipped());
        }
        let t1 = Instant::now();

        let job = unsafe { &mut *shared.job.get() };
        job.list = core::ptr::from_ref(list).cast();
        job.fb = Some(fb);
        job.rotation = self.rotation;
        job.buffers = self.strips.buffers(self.lanes).0;
        job.strips.clear();
        let hole = self.hole.unwrap_or_default();
        let over = self.over.filter(|_| !self.hold).unwrap_or_default();
        for region in self.damage[target].regions() {
            for area in region.subtract(&hole).chain([region.intersect(&over)]) {
                cut(area, fb.format(), self.buffer_bytes, ROW_GRID, &mut job.strips)?;
            }
        }
        stats.strips = job.strips.len() as u32;
        shared.next.store(0, Ordering::Relaxed);
        shared.frame.fetch_add(1, Ordering::Relaxed);
        // A lane on this task's core preempts it, so that lane starts last.
        for start in shared.start[..self.lanes].iter().rev() {
            start.give();
        }
        for _ in 0..self.lanes {
            shared.done.take();
        }
        self.damage[target].clear();
        for lane in 0..self.lanes {
            stats.raster_us += shared.raster_us[lane].load(Ordering::Relaxed);
            stats.stall_us += shared.stall_us[lane].load(Ordering::Relaxed);
        }
        let t2 = Instant::now();

        if !self.in_place() && !self.hold && self.frame_buffers() >= 2 {
            self.wait_flip()?;
            self.display.present(target)?;
            self.buffers.latest.store(target, Ordering::Release);
        }
        let t3 = Instant::now();
        stats.prepare_us = t1.duration_since(t0).as_micros() as u32;
        stats.draw_us = t2.duration_since(t1).as_micros() as u32;
        stats.flip_us = t3.duration_since(t2).as_micros() as u32;
        Ok(stats)
    }

    /// Blocks until the last rendered frame is on the panel.
    pub fn wait_visible(&self) -> Result<()> {
        self.wait_flip()
    }
}

impl Presenter for FbPresenter {
    fn area(&self) -> Rect {
        FbPresenter::area(self)
    }

    fn row_grid(&self) -> i16 {
        ROW_GRID
    }

    fn invalidate(&mut self, rect: Rect) {
        FbPresenter::invalidate(self, rect);
    }

    fn invalidate_all(&mut self) {
        FbPresenter::invalidate_all(self);
    }

    fn has_new_damage(&self) -> bool {
        self.fresh
    }

    fn acquire(&mut self) -> Result<()> {
        FbPresenter::acquire(self)
    }

    fn regions(&mut self) -> Result<&[Rect]> {
        self.acquire()?;
        let target = self.target.ok_or(Error::INVALID_STATE)?;
        Ok(self.damage[target].regions())
    }

    fn render(&mut self, list: &DisplayList<'_, Caps>, cache: &mut GlyphCache<Caps>) -> Result<Stats> {
        FbPresenter::render(self, list, cache)
    }

    fn set_rotation(&mut self, rotation: Rotation) -> Result<()> {
        self.rotation = rotation;
        self.reset()
    }

    fn set_strips(&mut self, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        FbPresenter::set_strips(self, strips, lanes)
    }

    fn changes_display(&self, config: &display::Config) -> bool {
        self.format_after(config) != self.display.pixel_format()
            || (config.fb_num != 0 && usize::from(config.fb_num) != self.display.frame_buffer_count())
    }

    fn fits_display(&self, config: &display::Config, strips: &Strips, lanes: usize) -> bool {
        self.fit(strips, lanes, self.format_after(config)).is_some()
    }

    unsafe fn set_display(&mut self, config: &display::Config, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        let Some(buffer_bytes) = self.fit(&strips, lanes, self.format_after(config)) else {
            return Err(strips);
        };
        (self.hole, self.hold, self.over) = (None, false, None);
        let result = unsafe { self.display.reconfigure(config) };
        self.buffers.latest.store(self.display.scanning().unwrap_or(0), Ordering::Release);
        self.buffers.back.store(NO_BACK, Ordering::Release);
        if let Err(err) = self.reset() {
            error!(TAG, "reset: {err}");
        }
        if let Err(err) = result {
            error!(TAG, "display: {err}");
            return Err(strips);
        }
        self.lanes = lanes;
        self.buffer_bytes = buffer_bytes;
        Ok(core::mem::replace(&mut self.strips, strips))
    }

    fn set_external(&mut self, hole: Option<Rect>, hold: bool, over: Option<Rect>) -> bool {
        let moved = hold != self.hold || hole.is_some() != self.hole.is_some();
        (self.hole, self.hold, self.over) = (hole, hold, over);
        if moved || hold {
            let target = self.target.take();
            let _ = self.acquire();
            return target != self.target;
        }
        false
    }

    fn behind(&mut self) -> bool {
        if !self.hold {
            return false;
        }
        self.target = None;
        let _ = self.acquire();
        self.target.is_some_and(|target| !self.damage[target].is_empty())
    }

    fn buffers(&self) -> Buffers {
        Buffers { count: self.frame_buffers(), ..self.buffers.clone() }
    }

    fn shown(&self) -> Option<FrameBuffer> {
        self.display.frame_buffer(self.latest() % self.frame_buffers())
    }

    fn blitter(&self) -> Option<NewBlitter> {
        Some(PpaBlitter::boxed)
    }
}
