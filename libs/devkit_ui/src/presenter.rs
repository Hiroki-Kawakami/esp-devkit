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

use bsp_rs::display::{self, Blitter, Display, FrameBuffer, PixelFormat, Rotation};
use devkit_gfx::{Bgr888, Damage, DisplayList, GlyphCache, Pixel, Prepared, Rect, Rgb565, Surface, draw};
use idf::heap::Caps;
use idf::log::error;
use idf::sync::Semaphore;
use idf::time::Instant;
use idf::{Error, Result, thread};

const TAG: &CStr = c"presenter";

const MAX_LANES: usize = 2;
const MAX_REGIONS: usize = 8;
const MAX_FRAME_BUFFERS: usize = 3;
const STRIPS_PER_LANE: usize = 2;
const LANE_STACK: usize = 8192;
const STALL_TIMEOUT: Duration = Duration::from_secs(1);
// The 2D engine's cost steps every 16 rows, and it hangs on short rotated
// copies, so damage and strips stay on this grid.
const ROW_GRID: i16 = 16;

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
    pub mode: PresentMode,
    /// At most this many framebuffers of the display.
    pub frame_buffers: usize,
    pub rotation: Rotation,
    /// Tasks drawing strips, one per core.
    pub lanes: usize,
    pub priority: u32,
    /// Internal RAM per strip buffer; each lane has two.
    pub strip_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: PresentMode::Swap,
            frame_buffers: MAX_FRAME_BUFFERS,
            rotation: Rotation::R0,
            lanes: MAX_LANES,
            priority: 10,
            strip_bytes: 64 * 1024,
        }
    }
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
struct Line([u8; 64]);

struct Job {
    list: *const DisplayList<'static, Caps>,
    strips: Vec<Rect, Caps>,
    fb: Option<FrameBuffer>,
    rotation: Rotation,
}

struct Shared {
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

struct Lane {
    id: usize,
    blitter: Blitter,
    buffers: [Vec<Line, Caps>; STRIPS_PER_LANE],
    turn: usize,
    last: Rect,
    raster: u32,
    stall: u32,
}

impl Lane {
    fn run(mut self, shared: Arc<Shared>) {
        loop {
            shared.start[self.id].take();
            // `Presenter::render` keeps the job and the list alive until `done`.
            let job = unsafe { &*shared.job.get() };
            let prepared = unsafe { &*shared.prepared.get() };
            let list = unsafe { &*job.list };
            (self.raster, self.stall) = (0, 0);
            match job.fb.map(|fb| fb.format()) {
                Some(PixelFormat::Rgb888) => self.strips::<Bgr888>(&shared, job, list, prepared),
                _ => self.strips::<Rgb565>(&shared, job, list, prepared),
            }
            while self.blitter.pending() > 0 {
                self.wait_blit();
            }
            shared.raster_us[self.id].store(self.raster, Ordering::Relaxed);
            shared.stall_us[self.id].store(self.stall, Ordering::Relaxed);
            shared.done.give();
        }
    }

    fn wait_blit(&mut self) {
        let t = Instant::now();
        while !self.blitter.wait_timeout(STALL_TIMEOUT) {
            error!(TAG, "lane {}: blit stalled, last {:?}", self.id, self.last);
        }
        self.stall += t.elapsed().as_micros() as u32;
    }

    fn strips<P: Pixel>(&mut self, shared: &Shared, job: &Job, list: &DisplayList<'static, Caps>, prepared: &Prepared<Caps>) {
        let Some(fb) = job.fb else { return };
        loop {
            let index = shared.next.fetch_add(1, Ordering::Relaxed);
            let Some(strip) = job.strips.get(index) else { return };
            if self.blitter.pending() == STRIPS_PER_LANE as u32 {
                self.wait_blit();
            }
            let t = Instant::now();
            let buffer = self.buffers[self.turn].as_mut_ptr().cast::<u8>();
            self.turn = (self.turn + 1) % STRIPS_PER_LANE;
            self.last = *strip;
            let surface = unsafe { Surface::new(buffer, strip.w as usize * P::BYTES, *strip) };
            draw::<P, _, _>(list, prepared, &surface);
            self.raster += t.elapsed().as_micros() as u32;
            let area = display::Rect {
                x: strip.x as u16,
                y: strip.y as u16,
                width: strip.w as u16,
                height: strip.h as u16,
            };
            if let Err(err) = unsafe { self.blitter.start(&fb, job.rotation, area, buffer) } {
                error!(TAG, "blit {strip:?}: {err}");
            }
        }
    }
}

/// Puts display lists on the panel: damaged rows are drawn into internal RAM
/// strips and copied, rotated as needed, into a framebuffer.
pub struct Presenter {
    display: &'static Display,
    shared: Arc<Shared>,
    damage: [Damage<MAX_REGIONS>; MAX_FRAME_BUFFERS],
    mode: PresentMode,
    frame_buffers: usize,
    rotation: Rotation,
    lanes: usize,
    strip_bytes: usize,
    latest: usize,
    target: Option<usize>,
}

impl Presenter {
    pub fn new(display: &'static Display, config: &Config) -> Result<Self> {
        if !(1..=MAX_LANES).contains(&config.lanes) || config.strip_bytes < 64 {
            return Err(Error::INVALID_ARG);
        }
        let shared = Arc::new(Shared {
            start: [Semaphore::try_new(0, 1)?, Semaphore::try_new(0, 1)?],
            done: Semaphore::try_new(0, MAX_LANES as u32)?,
            next: AtomicUsize::new(0),
            raster_us: [AtomicU32::new(0), AtomicU32::new(0)],
            stall_us: [AtomicU32::new(0), AtomicU32::new(0)],
            job: UnsafeCell::new(Job {
                list: core::ptr::null(),
                strips: Vec::with_capacity_in(64, Caps::INTERNAL),
                fb: None,
                rotation: Rotation::R0,
            }),
            prepared: UnsafeCell::new(Prepared::new_in(Caps::INTERNAL)),
        });
        for id in 0..MAX_LANES {
            let lines = config.strip_bytes / size_of::<Line>();
            let buffer = || {
                let mut v = Vec::new_in(Caps::DMA | Caps::INTERNAL);
                v.try_reserve_exact(lines).map_err(|_| Error::NO_MEM)?;
                v.resize(lines, Line([0; 64]));
                Ok::<_, Error>(v)
            };
            let lane = Lane {
                id,
                blitter: display.blitter(STRIPS_PER_LANE as u32)?,
                buffers: [buffer()?, buffer()?],
                turn: 0,
                last: Rect::default(),
                raster: 0,
                stall: 0,
            };
            let shared = shared.clone();
            thread::Builder::new()
                .name("ui_lane")
                .stack_size(LANE_STACK)
                .priority(config.priority)
                .core(Some(id as u32))
                .spawn(move || lane.run(shared))?;
        }
        let mut presenter = Self {
            display,
            shared,
            damage: [Damage::new(Rect::default()); MAX_FRAME_BUFFERS],
            mode: config.mode,
            frame_buffers: config.frame_buffers,
            rotation: config.rotation,
            lanes: config.lanes,
            strip_bytes: config.strip_bytes,
            latest: display.scanning().unwrap_or(0),
            target: None,
        };
        presenter.reset()?;
        Ok(presenter)
    }

    fn reset(&mut self) -> Result<()> {
        let area = self.area();
        self.damage = [Damage::new(area); MAX_FRAME_BUFFERS];
        self.invalidate_all();
        self.target = None;
        if self.mode == PresentMode::InPlace {
            self.latest %= self.frame_buffers();
            self.display.present(self.latest)?;
        }
        Ok(())
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

    /// The logical picture.
    pub fn area(&self) -> Rect {
        let size = self.display.size();
        let (w, h) = (size.width as i16, size.height as i16);
        match self.rotation {
            Rotation::R0 | Rotation::R180 => Rect::new(0, 0, w, h),
            Rotation::R90 | Rotation::R270 => Rect::new(0, 0, h, w),
        }
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
        while self.display.scanning().is_some_and(|i| i != self.latest) {
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
        let target = if self.mode == PresentMode::InPlace || count < 2 {
            self.latest
        } else if count == 2 {
            self.wait_flip()?;
            (self.latest + 1) % count
        } else {
            let scanning = self.display.scanning().unwrap_or(self.latest);
            (0..count).find(|&i| i != scanning && i != self.latest).unwrap_or((self.latest + 1) % count)
        };
        self.target = Some(target);
        Ok(())
    }

    /// Draws what was invalidated and shows it. `Swap` returns once the frame
    /// is queued for the next boundary.
    pub fn render<C: Allocator + Clone>(&mut self, list: &DisplayList<'_, Caps>, cache: &mut GlyphCache<C>) -> Result<Stats> {
        self.acquire()?;
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
        let t1 = Instant::now();

        let job = unsafe { &mut *shared.job.get() };
        job.list = core::ptr::from_ref(list).cast();
        job.fb = Some(fb);
        job.rotation = self.rotation;
        job.strips.clear();
        for area in self.damage[target].regions() {
            let stride = area.w as usize * fb.format().bytes();
            let rows = ((self.strip_bytes / stride) as i16 / ROW_GRID * ROW_GRID).max(ROW_GRID);
            let mut y = area.y;
            while y < area.bottom() {
                let h = rows.min(area.bottom() - y);
                job.strips.push(Rect::new(area.x, y, area.w, h));
                y += h;
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
        self.damage[target].clear();
        for lane in 0..self.lanes {
            stats.raster_us += shared.raster_us[lane].load(Ordering::Relaxed);
            stats.stall_us += shared.stall_us[lane].load(Ordering::Relaxed);
        }
        let t2 = Instant::now();

        if self.mode == PresentMode::Swap && self.frame_buffers() >= 2 {
            self.wait_flip()?;
            self.display.present(target)?;
            self.latest = target;
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
