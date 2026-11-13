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
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use core::time::Duration;

use bsp_rs::display::{self, Display, FrameBuffer, ImageFormat, Rotation};
use devkit_gfx::{Argb8888, Bgr888, ClipStore, Damage, DisplayList, GlyphCache, Pixel, Prepared, Rect, Rgb565, Size, Surface, draw, draw_from};
use idf::heap::Caps;
use idf::log::{error, warn};
use idf::sync::Semaphore;
use idf::time::Instant;
use idf::{Error, Result, thread};

use media_engine::{Slot, Srm};

use crate::blit::{self, Blits};
use crate::external::{Buffers, Core, NO_BACK, Picture};

const TAG: &CStr = c"presenter";

const MAX_LANES: usize = 2;
const MAX_REGIONS: usize = 8;
const MAX_FRAME_BUFFERS: usize = 3;
const STRIPS_PER_LANE: usize = 2;
const LANE_STACK: usize = 8192;
const STALL_TIMEOUT: Duration = Duration::from_secs(1);
// The 2D engine's cost steps every 16 rows, and it hangs on short rotated
// copies, so damage and strips stay on this grid.
pub(crate) const ROW_GRID: i16 = 16;

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
    /// Split evenly into two buffers per lane.
    pub strips: StripSource,
    /// The presenter's own lists. Memory that may stay on one heap for the whole
    /// UI; the default fits boards with PSRAM.
    pub data_caps: Caps,
    /// Never memory that can be RTC RAM, as `INTERNAL` alone can: lanes run PIE
    /// code, whose saved registers RTC RAM cannot take. `SIMD` alone avoids it.
    pub lane_stack_caps: Caps,
}

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
            lane_stack_caps: Caps::SIMD,
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
struct Line([u8; 64]);

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
    blits: Blits,
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
            // `Presenter::render` keeps the job and the list alive until `done`.
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
        while !self.blits.wait_timeout(STALL_TIMEOUT) {
            error!(
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
            );
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
            let mut op = Srm::new(unsafe { blit::image(buffer, fb.format(), size) });
            op.at = (strip.x, strip.y);
            if let Err(err) = self.blits.run(target, &op) {
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
    buffers: Buffers,
    hole: Option<Rect>,
    hold: bool,
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

fn holds_rows(display: &Display, format: ImageFormat, rotation: Rotation, buffer_bytes: usize) -> bool {
    let stride = format.row_bytes(area(display, rotation).w as usize);
    if buffer_bytes < stride * ROW_GRID as usize {
        error!(TAG, "strips of {buffer_bytes} bytes hold no 16 rows of {stride}");
        return false;
    }
    true
}

impl Presenter {
    /// Refused when a strip buffer cannot hold 16 rows of the logical width.
    pub fn new(display: &'static Display, config: Config) -> Result<Self> {
        if !(1..=MAX_LANES).contains(&config.lanes) {
            return Err(Error::INVALID_ARG);
        }
        let strips = match config.strips {
            StripSource::Allocate { bytes, caps } => Strips::allocate(bytes, caps)?,
            StripSource::Given(strips) => strips,
        };
        let (_, buffer_bytes) = strips.buffers(config.lanes);
        if !holds_rows(display, display.pixel_format(), config.rotation, buffer_bytes) {
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
                blits: Blits::new(STRIPS_PER_LANE)?,
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

    /// What a surface needs to share the framebuffers with this presenter.
    pub(crate) fn buffers(&self) -> Buffers {
        Buffers { count: self.frame_buffers(), ..self.buffers.clone() }
    }

    /// Leaves `hole` undrawn. With `hold`, frames go to the back framebuffer and
    /// the surface presents them; otherwise a hole draws in place. `true` when
    /// the next frame goes elsewhere than `regions` said.
    pub(crate) fn set_external(&mut self, hole: Option<Rect>, hold: bool) -> bool {
        let moved = hold != self.hold || hole.is_some() != self.hole.is_some();
        (self.hole, self.hold) = (hole, hold);
        if moved || hold {
            let target = self.target.take();
            let _ = self.acquire();
            return target != self.target;
        }
        false
    }

    /// While a surface presents: whether the framebuffer it takes next lacks
    /// something the ones before it were given.
    pub(crate) fn behind(&mut self) -> bool {
        if !self.hold {
            return false;
        }
        self.target = None;
        let _ = self.acquire();
        self.target.is_some_and(|target| !self.damage[target].is_empty())
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
        holds_rows(self.display, format, self.rotation, buffer_bytes).then_some(buffer_bytes)
    }

    fn format_after(&self, config: &display::Config) -> ImageFormat {
        config.pixel_format.unwrap_or_else(|| self.display.pixel_format())
    }

    /// Whether `config` asks the display for something it is not.
    pub fn changes_display(&self, config: &display::Config) -> bool {
        self.format_after(config) != self.display.pixel_format()
            || (config.fb_num != 0 && usize::from(config.fb_num) != self.display.frame_buffer_count())
    }

    /// Whether `set_display` would take `strips` for `config`.
    pub fn fits_display(&self, config: &display::Config, strips: &Strips, lanes: usize) -> bool {
        self.fit(strips, lanes, self.format_after(config)).is_some()
    }

    /// `Display::reconfigure`, then `set_strips`, and everything is repainted.
    /// Refused without touching the display when the strips would not hold 16
    /// rows; when the display fails, `strips` come back too. Nothing else may
    /// use the display or its framebuffers meanwhile.
    pub unsafe fn set_display(&mut self, config: &display::Config, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        let Some(buffer_bytes) = self.fit(&strips, lanes, self.format_after(config)) else {
            return Err(strips);
        };
        (self.hole, self.hold) = (None, false);
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

    /// Turns the picture from the next frame on, which repaints everything.
    /// Refused when a buffer cannot hold 16 rows of the new logical width.
    pub fn set_rotation(&mut self, rotation: Rotation) -> Result<()> {
        if !holds_rows(self.display, self.display.pixel_format(), rotation, self.buffer_bytes) {
            return Err(Error::INVALID_SIZE);
        }
        self.rotation = rotation;
        self.reset()
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
    /// What the next `render` repaints. May wait for a framebuffer.
    pub fn regions(&mut self) -> Result<&[Rect]> {
        self.acquire()?;
        let target = self.target.ok_or(Error::INVALID_STATE)?;
        Ok(self.damage[target].regions())
    }

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
        for area in self.damage[target].regions().iter().flat_map(|r| r.subtract(&hole)) {
            let stride = fb.format().row_bytes(area.w as usize);
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

    /// Draws what `list` paints from `first` on inside `rect` into a picture to
    /// lay over a surface. Runs on this task; it is only redone when that UI
    /// changes.
    pub(crate) fn render_overlay<C: Allocator + Clone>(
        &mut self,
        list: &DisplayList<'_, Caps>,
        first: usize,
        rect: Rect,
        cache: &mut GlyphCache<C>,
    ) -> Result<Picture> {
        let bytes = rect.area() as usize * Argb8888::BYTES;
        let memory = Strips::allocate((bytes + size_of::<Line>()) * STRIPS_PER_LANE, Caps::SPIRAM)?;
        let [[drawn, turned], _] = memory.buffers(1).0;
        let shared = &*self.shared;
        let prepared = unsafe { &mut *shared.prepared.get() };
        prepared.prepare(list, &[rect], cache);
        let surface = unsafe { Surface::new(drawn, rect.w as usize * Argb8888::BYTES, rect) };
        draw_from::<Argb8888, _, _, _>(list, first, prepared, &mut ClipStore::new_in(Caps::SPIRAM), &surface);
        let size = Size::new(rect.w as u16, rect.h as u16);
        let opaque = opaque_core(unsafe { core::slice::from_raw_parts(drawn, bytes) }, size).filter(|c| c.area() * 2 >= rect.area());
        let turned_size = if self.rotation.swaps_axes() { Size::new(size.height, size.width) } else { size };
        let pixels = if self.rotation == Rotation::R0 {
            drawn
        } else {
            let mut op = Srm::new(unsafe { blit::image(drawn, ImageFormat::Argb8888, size) });
            op.rotation = self.rotation;
            let mut target = unsafe { blit::target(turned, ImageFormat::Argb8888, turned_size) }?;
            let mut slot = Slot::new();
            let mut job = media_engine::ppa()?.srm(&op, &mut target, &mut slot)?;
            job.wait_timeout(STALL_TIMEOUT).ok_or(Error::TIMEOUT)??;
            turned
        };
        let core = opaque.and_then(|local| match self.copy_core(pixels, turned_size, local) {
            Ok(core) => Some(core),
            Err(err) => {
                error!(TAG, "overlay core: {err}");
                None
            }
        });
        Ok(Picture { _memory: memory, pixels, size, rotation: self.rotation, core })
    }

    /// `local` of a picture turned for the panel at `pixels`, in the panel's
    /// format.
    fn copy_core(&self, pixels: *mut u8, turned_size: Size, local: Rect) -> Result<Core> {
        let format = self.display.pixel_format();
        let src = unsafe { blit::target(pixels, ImageFormat::Argb8888, turned_size) }?.rotated(self.rotation).stored_rect(local).ok_or(Error::INVALID_ARG)?;
        let line = size_of::<Line>();
        let memory = Strips::allocate(format.row_bytes(src.w as usize) * src.h as usize + line * 2, Caps::SPIRAM)?;
        let core = unsafe { memory.ptr.add(memory.ptr.align_offset(line)) };
        let mut op = Srm::new(unsafe { blit::image(pixels, ImageFormat::Argb8888, turned_size) });
        op.src_rect = src;
        let mut target = unsafe { blit::target(core, format, Size::new(src.w as u16, src.h as u16)) }?;
        let mut slot = Slot::new();
        let mut job = media_engine::ppa()?.srm(&op, &mut target, &mut slot)?;
        job.wait_timeout(STALL_TIMEOUT).ok_or(Error::TIMEOUT)??;
        Ok(Core { _memory: memory, pixels: core, format, rect: local })
    }

    /// The framebuffer with the last frame.
    pub(crate) fn shown(&self) -> Option<FrameBuffer> {
        self.display.frame_buffer(self.latest() % self.frame_buffers())
    }

    /// Blocks until the last rendered frame is on the panel.
    pub fn wait_visible(&self) -> Result<()> {
        self.wait_flip()
    }
}

/// The largest rectangle of opaque pixels in ARGB `pixels` of `size`.
pub(crate) fn opaque_core(pixels: &[u8], size: Size) -> Option<Rect> {
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    let runs: Vec<(usize, usize)> = (0..height)
        .map(|y| {
            let row = &pixels[y * width * Argb8888::BYTES..][..width * Argb8888::BYTES];
            let (mut best, mut start) = ((0, 0), None);
            for (x, pixel) in row.chunks_exact(Argb8888::BYTES).enumerate() {
                match (pixel[3] == 0xff, start) {
                    (true, None) => start = Some(x),
                    (false, Some(from)) => {
                        if x - from > best.1 - best.0 {
                            best = (from, x);
                        }
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(from) = start
                && width - from > best.1 - best.0
            {
                best = (from, width);
            }
            best
        })
        .collect();
    let mut best: Option<(usize, Rect)> = None;
    for top in 0..height {
        let (mut left, mut right) = runs[top];
        if (right - left) * (height - top) <= best.map_or(0, |b| b.0) {
            continue;
        }
        for (bottom, &(l, r)) in runs.iter().enumerate().skip(top) {
            (left, right) = (left.max(l), right.min(r));
            if right <= left {
                break;
            }
            let area = (right - left) * (bottom + 1 - top);
            if area > best.map_or(0, |b| b.0) {
                best = Some((area, Rect::new(left as i16, top as i16, (right - left) as i16, (bottom + 1 - top) as i16)));
            }
        }
    }
    best.map(|b| b.1)
}
