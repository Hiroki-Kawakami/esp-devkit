/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::alloc::Allocator;
use core::any::Any;
use core::cell::UnsafeCell;
use core::ffi::CStr;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use core::time::Duration;

use bsp_rs::display::{self, Blitter, Display, FrameBuffer, PixelFormat, Rotation};
use devkit::Lease;
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
    /// Strip memory in total, split evenly into two buffers per lane.
    pub strip_bytes: usize,
    pub strip_caps: Caps,
    /// The presenter's own lists. Memory that may stay on one heap for the whole
    /// UI; the default fits boards with PSRAM.
    pub data_caps: Caps,
    /// `None` is internal RAM.
    pub lane_stack_caps: Option<Caps>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: PresentMode::Swap,
            frame_buffers: MAX_FRAME_BUFFERS,
            rotation: Rotation::R0,
            lanes: MAX_LANES,
            priority: 10,
            strip_bytes: 4 * 64 * 1024,
            strip_caps: Caps::DMA | Caps::INTERNAL,
            data_caps: Caps::SPIRAM,
            lane_stack_caps: None,
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

/// Memory strips can be drawn in.
pub trait StripMemory: Send + 'static {
    fn bytes(&mut self) -> &mut [u8];
}

impl<T: AsMut<[u8]> + Send> StripMemory for Lease<'static, T> {
    fn bytes(&mut self) -> &mut [u8] {
        (**self).as_mut()
    }
}

struct OwnedStrips(Vec<Line, Caps>);

impl StripMemory for OwnedStrips {
    fn bytes(&mut self) -> &mut [u8] {
        let bytes = self.0.len() * size_of::<Line>();
        unsafe { core::slice::from_raw_parts_mut(self.0.as_mut_ptr().cast(), bytes) }
    }
}

/// The memory a presenter draws strips in, handed over with `Presenter::set_strips`.
pub struct Strips {
    memory: Box<dyn AnyStrips>,
    ptr: *mut u8,
    len: usize,
}

unsafe impl Send for Strips {}

trait AnyStrips: Send {
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<M: StripMemory> AnyStrips for M {
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl Strips {
    pub fn new<M: StripMemory>(memory: M) -> Self {
        let mut memory = Box::new(memory);
        let bytes = memory.bytes();
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
    pub fn into_inner<M: StripMemory>(self) -> Option<M> {
        self.memory.into_any().downcast::<M>().ok().map(|m| *m)
    }

    /// Two aligned buffers per lane.
    fn buffers(&self, lanes: usize) -> ([[*mut u8; STRIPS_PER_LANE]; MAX_LANES], usize) {
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
    blitter: Blitter,
    turn: usize,
    last: Rect,
    before: Rect,
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
                self.wait_blit(&shared, job);
            }
            shared.raster_us[self.id].store(self.raster, Ordering::Relaxed);
            shared.stall_us[self.id].store(self.stall, Ordering::Relaxed);
            shared.done.give();
        }
    }

    fn wait_blit(&mut self, shared: &Shared, job: &Job) {
        let t = Instant::now();
        while !self.blitter.wait_timeout(STALL_TIMEOUT) {
            error!(
                TAG,
                "lane {}: blit stalled, last {:?}, before {:?}, pending {}, strip {}/{}, frame {}, {:?}",
                self.id,
                self.last,
                self.before,
                self.blitter.pending(),
                shared.next.load(Ordering::Relaxed),
                job.strips.len(),
                shared.frame.load(Ordering::Relaxed),
                job.rotation,
            );
        }
        self.stall += t.elapsed().as_micros() as u32;
    }

    fn strips<P: Pixel>(&mut self, shared: &Shared, job: &Job, list: &DisplayList<'static, Caps>, prepared: &Prepared<Caps>) {
        let Some(fb) = job.fb else { return };
        loop {
            let index = shared.next.fetch_add(1, Ordering::Relaxed);
            let Some(strip) = job.strips.get(index) else { return };
            if self.blitter.pending() == STRIPS_PER_LANE as u32 {
                self.wait_blit(shared, job);
            }
            let t = Instant::now();
            let buffer = job.buffers[self.id][self.turn];
            self.turn = (self.turn + 1) % STRIPS_PER_LANE;
            self.before = self.last;
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
    strips: Strips,
    buffer_bytes: usize,
    latest: usize,
    target: Option<usize>,
    fresh: bool,
}

impl Presenter {
    pub fn new(display: &'static Display, config: &Config) -> Result<Self> {
        if !(1..=MAX_LANES).contains(&config.lanes) {
            return Err(Error::INVALID_ARG);
        }
        let strips = Strips::allocate(config.strip_bytes, config.strip_caps)?;
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
                blitter: display.blitter(STRIPS_PER_LANE as u32)?,
                turn: 0,
                last: Rect::default(),
                before: Rect::default(),
                raster: 0,
                stall: 0,
            };
            let shared = shared.clone();
            let mut builder = thread::Builder::new()
                .name("ui_lane")
                .stack_size(LANE_STACK)
                .priority(config.priority)
                .core(Some(id as u32));
            if let Some(caps) = config.lane_stack_caps {
                builder = builder.stack_caps(caps);
            }
            builder.spawn(move || lane.run(shared))?;
        }
        let (_, buffer_bytes) = strips.buffers(config.lanes);
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
            latest: display.scanning().unwrap_or(0),
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

    /// Draws in `strips` from the next frame on, with `lanes` lanes, and returns
    /// the memory used so far: nothing draws in it any more. Refused, handing
    /// `strips` back, when a buffer cannot hold 16 rows of the logical width.
    pub fn set_strips(&mut self, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        let (_, buffer_bytes) = strips.buffers(lanes.clamp(1, MAX_LANES));
        let stride = self.area().w as usize * self.display.pixel_format().bytes();
        if !(1..=MAX_LANES).contains(&lanes) || buffer_bytes < stride * ROW_GRID as usize {
            error!(TAG, "strips of {buffer_bytes} bytes hold no 16 rows of {stride}");
            return Err(strips);
        }
        self.lanes = lanes;
        self.buffer_bytes = buffer_bytes;
        Ok(core::mem::replace(&mut self.strips, strips))
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

    /// Whether a frame is due without new damage: the first one, or one after
    /// `configure`.
    pub fn has_new_damage(&self) -> bool {
        self.fresh
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
        let t1 = Instant::now();

        let job = unsafe { &mut *shared.job.get() };
        job.list = core::ptr::from_ref(list).cast();
        job.fb = Some(fb);
        job.rotation = self.rotation;
        job.buffers = self.strips.buffers(self.lanes).0;
        job.strips.clear();
        for area in self.damage[target].regions() {
            let stride = area.w as usize * fb.format().bytes();
            let rows = (self.buffer_bytes / stride).min(i16::MAX as usize) as i16 / ROW_GRID * ROW_GRID;
            if rows == 0 {
                return Err(Error::INVALID_SIZE);
            }
            let mut y = area.y;
            while y < area.bottom() {
                let h = rows.min(area.bottom() - y);
                job.strips.push(Rect::new(area.x, y, area.w, h));
                y += h;
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
