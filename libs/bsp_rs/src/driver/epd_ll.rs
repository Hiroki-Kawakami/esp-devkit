/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! A grayscale EPD without a controller, driven line by line from an 8-bit
//! i80 bus: the source data on D0..D7, STH on CS and CL on WR, with CKV, SPV,
//! LE, OE and the power rail on GPIOs. Each pixel replays its own waveform
//! (`epd_waveform`) on a task of its own.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::{CStr, c_void};
use core::slice;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

use idf::heap::{self, Caps};
use idf::lcd::i80::{self, DmaBuffer, Io};
use idf::log::error;
use idf::sync::{Mutex, MutexGuard, Semaphore};
use idf::time::delay_us;
use idf::{Error, Result, gpio, sys, system, thread};

use super::epd_waveform::{self as wf, B1_IDLE, B1_PENDING, Frame, Pixel, SLOTS, Waveform};
use crate::Absent;
use crate::display::{self, Driver, EpdDriver, EpdMode, GramDriver, Image, ImageFormat, Rect, Rotation, Size, Ticket};

const TAG: &CStr = c"epd_ll";

const SLOT_CLEAR: usize = 3;
const QUEUE_DEPTH: usize = 4;
const DMA_BURST: usize = 64;
/// Most distinct frames one frame flattens; beyond it the scan reads the
/// waveforms themselves.
const ACT_POOL: usize = 16;
const ACT_HOLD: usize = ACT_POOL;
const TASK_STACK: usize = 6144;

#[derive(Clone, Copy)]
pub(crate) struct ControlPins {
    pub ckv: i32,
    pub spv: i32,
    pub le: i32,
    pub oe: i32,
    pub pwr: i32,
}

pub(crate) struct Waveforms {
    pub fast: Option<&'static [Frame]>,
    pub quality: Option<&'static [Frame]>,
    pub text: Option<&'static [Frame]>,
    /// Drives everything to white; `Epd::clear` replays it.
    pub clear: &'static [Frame],
}

pub(crate) struct Config {
    pub data: [i32; 8],
    pub sph: i32,
    pub cl: i32,
    /// The bus takes a D/C pin the panel lacks: a spare one, or one of
    /// `control` that is set again once the bus is up.
    pub dc_dummy: i32,
    pub control: ControlPins,
    pub pclk_hz: u32,
    pub size: Size,
    pub ppi: u16,
    pub line_bytes: u16,
    /// Clocked out after each line, holding CKV while the gates settle.
    pub line_padding: u8,
    pub waveforms: Waveforms,
    pub priority: u32,
    /// Of the refresh task; the bus interrupt goes to another core.
    pub core: Option<u32>,
}

#[inline(always)]
fn level(pin: i32, high: bool) {
    unsafe { sys::gpio_set_level(pin as _, u32::from(high)) };
}

/// Read by the bus interrupt.
struct Latch {
    ckv: i32,
    le: i32,
    busy: AtomicBool,
}

// The bus interrupt is not allocated IRAM-safe, so this may stay in flash.
unsafe extern "C" fn line_sent(_io: sys::esp_lcd_panel_io_handle_t, _data: *mut sys::esp_lcd_panel_io_event_data_t, ctx: *mut c_void) -> bool {
    let latch = unsafe { &*ctx.cast::<Latch>() };
    level(latch.ckv, false);
    level(latch.le, true);
    latch.busy.store(false, Ordering::Release);
    false
}

struct Bus {
    io: Io,
    latch: Box<Latch>,
    pins: ControlPins,
    tx_len: usize,
}

impl Bus {
    fn new(config: &Config) -> Result<Self> {
        let pins = config.control;
        gpio::configure(&[pins.ckv, pins.spv, pins.le, pins.oe, pins.pwr], gpio::Mode::Output)?;
        for pin in [pins.ckv, pins.spv, pins.le, pins.oe, pins.pwr] {
            level(pin, false);
        }
        for &pin in config.data.iter().chain(&[config.cl, config.sph]) {
            gpio::set_direction(pin, gpio::Mode::Output)?;
        }
        let tx_len = usize::from(config.line_bytes) + usize::from(config.line_padding);
        let bus = i80::Bus::new(&i80::BusConfig {
            dc: config.dc_dummy,
            wr: config.cl,
            data: &config.data,
            max_transfer_bytes: tx_len,
            dma_burst_size: DMA_BURST,
        })?;
        // Back to a plain output; resetting the pin would pull it up for a
        // moment, a glitch on a power rail.
        gpio::set_direction(config.dc_dummy, gpio::Mode::Output)?;
        let latch = Box::new(Latch { ckv: pins.ckv, le: pins.le, busy: AtomicBool::new(false) });
        let ctx = (&*latch as *const Latch).cast_mut().cast();
        let io = unsafe {
            bus.new_io(
                &i80::IoConfig {
                    cs: config.sph,
                    pclk_hz: config.pclk_hz,
                    queue_depth: QUEUE_DEPTH,
                    cmd_bits: 0,
                    param_bits: 0,
                    dc_levels: i80::DcLevels { data: true, ..Default::default() },
                    flags: i80::IoFlags { pclk_idle_low: true, ..Default::default() },
                },
                Some(line_sent),
                ctx,
            )
        }?;
        Ok(Self { io, latch, pins, tx_len })
    }

    /// The bus interrupt lands on the core that creates the bus, and runs
    /// inline in `tx_color` there: on the other core it overlaps the next
    /// line's packing instead.
    fn new_off(config: &Config) -> Result<Self> {
        let cores = system::CORE_COUNT;
        let core = match config.core {
            Some(core) if core < cores => (core + 1) % cores,
            _ => (system::core_id() + 1) % cores,
        };
        thread::scope(|scope| {
            match thread::Builder::new().name("epd_bus").stack_size(4096).core(Some(core)).spawn_scoped(scope, || Self::new(config)) {
                Ok(handle) => handle.join(),
                Err(_) => Self::new(config),
            }
        })
    }

    fn wait(&self) {
        while self.latch.busy.load(Ordering::Acquire) {
            thread::yield_now();
        }
    }

    fn power_on(&self) {
        level(self.pins.oe, true);
        delay_us(100);
        level(self.pins.pwr, true);
        delay_us(100);
    }

    fn power_off(&self) {
        level(self.pins.pwr, false);
        delay_us(1000);
        level(self.pins.oe, false);
        delay_us(100);
    }

    /// Scans the gate driver back to the top row.
    fn frame_begin(&self) {
        let p = &self.pins;
        self.wait();
        level(p.spv, false);
        delay_us(1);
        level(p.ckv, false);
        delay_us(3);
        level(p.ckv, true);
        delay_us(1);
        level(p.spv, true);
        for _ in 0..3 {
            delay_us(3);
            level(p.ckv, false);
            delay_us(3);
            level(p.ckv, true);
        }
    }

    /// Waits for the line before, which the bus interrupt latched; `line`
    /// stays in use until the next call.
    fn write_line(&self, line: &DmaBuffer) {
        self.wait();
        self.latch.busy.store(true, Ordering::Release);
        level(self.pins.le, false);
        level(self.pins.ckv, true);
        let data = unsafe { slice::from_raw_parts(line.as_ptr(), self.tx_len) };
        if let Err(err) = unsafe { self.io.tx_color(-1, data) } {
            error!(TAG, "line: {err}");
            self.latch.busy.store(false, Ordering::Release);
        }
    }

    fn frame_end(&self) {
        self.wait();
        level(self.pins.le, false);
        level(self.pins.ckv, true);
    }
}

/// Written under `Shared::lock`.
struct Counters {
    /// Pixels per `b1`, idle ones aside.
    group: [u32; 256],
    active_px: u32,
    /// Mod 64.
    frame: u8,
    /// The task is driving rather than waiting on `wake`.
    running: bool,
    mode: EpdMode,
}

struct Shared {
    width: usize,
    height: usize,
    wf: [Option<Waveform>; SLOTS],
    /// Written under `lock`; the scan reads it without.
    state: Box<[Pixel], Caps>,
    /// Pixels per row that are not idle; written under `lock`.
    row_nonidle: Box<[AtomicU16], Caps>,
    lock: Mutex<Counters>,
    waiters: AtomicU32,
    /// Given every frame something finishes or someone waits.
    retired: Semaphore,
    wake: Semaphore,
}

fn add(cell: &AtomicU16, n: u16) {
    cell.store(cell.load(Ordering::Relaxed).wrapping_add(n), Ordering::Relaxed);
}

fn sub(cell: &AtomicU16, n: u16) {
    cell.store(cell.load(Ordering::Relaxed).wrapping_sub(n), Ordering::Relaxed);
}

impl Shared {
    fn row(&self, y: usize) -> &[Pixel] {
        &self.state[y * self.width..][..self.width]
    }

    /// Lets the frames run once, holding `lock` again after.
    fn wait_retire<'a>(&'a self, guard: MutexGuard<'a, Counters>) -> MutexGuard<'a, Counters> {
        self.waiters.fetch_add(1, Ordering::SeqCst);
        drop(guard);
        self.retired.take();
        let guard = self.lock.lock();
        self.waiters.fetch_sub(1, Ordering::SeqCst);
        guard
    }

    fn area_has_active(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> bool {
        (y0..y1).any(|y| {
            self.row_nonidle[y].load(Ordering::Relaxed) != 0
                && self.row(y)[x0..x1].iter().any(|px| wf::is_active(px.load(Ordering::Relaxed)))
        })
    }

    fn start(&self, counters: &mut Counters) -> bool {
        let wake = !counters.running;
        counters.running = true;
        wake
    }

    /// `pixels` are L8 rows `stride` bytes apart.
    fn draw(&self, area: Rect, pixels: &[u8], stride: usize) -> Result<()> {
        let (x0, y0, w, h) = (area.x as usize, area.y as usize, area.w as usize, area.h as usize);
        let mut guard = self.lock.lock();
        let mode = guard.mode;
        if mode == EpdMode::Seed {
            while self.area_has_active(x0, y0, x0 + w, y0 + h) {
                guard = self.wait_retire(guard);
            }
            for r in 0..h {
                let src = &pixels[r * stride..][..w];
                let mut subsumed = 0;
                for (px, &g) in self.row(y0 + r)[x0..x0 + w].iter().zip(src) {
                    subsumed += u16::from((px.load(Ordering::Relaxed) >> 8) as u8 == B1_PENDING);
                    let g = u16::from(g >> 4);
                    px.store(0xFF00 | g << 4 | g, Ordering::Relaxed);
                }
                sub(&self.row_nonidle[y0 + r], subsumed);
                guard.group[usize::from(B1_PENDING)] -= u32::from(subsumed);
            }
            return Ok(());
        }
        loop {
            let mut conflict = false;
            for r in 0..h {
                let src = &pixels[r * stride..][..w];
                let mut gained = 0;
                for (px, &g) in self.row(y0 + r)[x0..x0 + w].iter().zip(src) {
                    let drawn = wf::draw_px(px.load(Ordering::Relaxed), g >> 4);
                    px.store(drawn.px, Ordering::Relaxed);
                    conflict |= drawn.conflict;
                    gained += u16::from(drawn.now_nonidle);
                }
                add(&self.row_nonidle[y0 + r], gained);
                guard.group[usize::from(B1_PENDING)] += u32::from(gained);
            }
            if !conflict {
                break;
            }
            guard = self.wait_retire(guard);
        }
        drop(guard);
        match mode {
            EpdMode::Refresh(waveform) => self.refresh(area, waveform, false),
            _ => Ok(()),
        }
    }

    fn refresh(&self, area: Rect, waveform: display::Waveform, all: bool) -> Result<()> {
        let slot = match waveform {
            display::Waveform::Fast => 0,
            display::Waveform::Quality => 1,
            display::Waveform::Text => 2,
        };
        if self.wf[slot].is_none() {
            return Ok(());
        }
        let (x0, y0) = (area.x as usize, area.y as usize);
        let (x1, y1) = (x0 + area.w as usize, y0 + area.h as usize);
        let mut guard = self.lock.lock();
        if all {
            while self.area_has_active(x0, y0, x1, y1) {
                guard = self.wait_retire(guard);
            }
        }
        let b1 = wf::armed(slot, guard.frame);
        let mut armed = 0;
        for y in y0..y1 {
            if !all && self.row_nonidle[y].load(Ordering::Relaxed) == 0 {
                continue;
            }
            let (mut row_armed, mut new_nonidle) = (0u32, 0u16);
            for px in &self.row(y)[x0..x1] {
                let v = px.load(Ordering::Relaxed);
                let pb1 = (v >> 8) as u8;
                if pb1 == B1_PENDING {
                    guard.group[usize::from(B1_PENDING)] -= 1;
                } else if !(all && pb1 == B1_IDLE) {
                    continue;
                } else {
                    new_nonidle += 1;
                }
                px.store((v & 0x00FF) | u16::from(b1) << 8, Ordering::Relaxed);
                row_armed += 1;
            }
            add(&self.row_nonidle[y], new_nonidle);
            armed += row_armed;
        }
        let mut wake = false;
        if armed != 0 {
            guard.group[usize::from(b1)] += armed;
            guard.active_px += armed;
            wake = self.start(&mut guard);
        }
        drop(guard);
        if wake {
            self.wake.give();
        }
        Ok(())
    }

    fn clear(&self) {
        let mut guard = self.lock.lock();
        while guard.active_px != 0 {
            guard = self.wait_retire(guard);
        }
        // Armed at 61 or 62, its b1 would be a marker; nothing replays, so the
        // frame count is free to move.
        if matches!(guard.frame, 61 | 62) {
            guard.frame = 63;
        }
        let b1 = wf::armed(SLOT_CLEAR, guard.frame);
        for px in self.state.iter() {
            px.store(u16::from(b1) << 8 | (px.load(Ordering::Relaxed) & 0x00F0) | 15, Ordering::Relaxed);
        }
        for row in self.row_nonidle.iter() {
            row.store(self.width as u16, Ordering::Relaxed);
        }
        let total = (self.width * self.height) as u32;
        guard.group[usize::from(B1_PENDING)] = 0;
        guard.group[usize::from(b1)] = total;
        guard.active_px = total;
        let wake = self.start(&mut guard);
        drop(guard);
        if wake {
            self.wake.give();
        }
    }

    fn wait_idle(&self) {
        let mut guard = self.lock.lock();
        while guard.active_px != 0 {
            guard = self.wait_retire(guard);
        }
    }
}

/// What only the refresh task touches.
struct Scan {
    bus: Bus,
    rowset_tab: [&'static Frame; 256],
    retire_tab: [bool; 256],
    act_index: [usize; 256],
    /// Internal RAM, so the scan never reaches the waveforms in flash; the
    /// last one stays all hold.
    act_pool: Box<[[u8; 256]; ACT_POOL + 1], Caps>,
    lines: [DmaBuffer; 2],
    hold_line: DmaBuffer,
    uni_line: DmaBuffer,
    line_bytes: usize,
}

fn run_frame(shared: &Shared, scan: &mut Scan) {
    let (w, h) = (shared.width, shared.height);
    let mut guard = shared.lock.lock();
    guard.frame = (guard.frame + 1) & 63;
    let skip_settle = shared.waiters.load(Ordering::SeqCst) > 0;
    wf::build_frame_tab(&shared.wf, guard.frame, skip_settle, &mut scan.rowset_tab, &mut scan.retire_tab);

    // Whether anything finishes, and whether one frame that drives every
    // pixel alike is all that replays, so one line serves every full row.
    let (mut any_retire, mut multiple) = (false, false);
    let mut only: Option<&Frame> = None;
    for b1 in 0..256 {
        if guard.group[b1] == 0 {
            continue;
        }
        any_retire |= scan.retire_tab[b1];
        match only {
            None => only = Some(scan.rowset_tab[b1]),
            Some(f) => multiple |= !core::ptr::eq(f, scan.rowset_tab[b1]),
        }
    }
    let uniform = match only {
        Some(f) if !multiple && guard.group[usize::from(B1_PENDING)] == 0 => wf::rowset_uniform(f),
        _ => None,
    };
    if let Some(byte) = uniform {
        unsafe { scan.uni_line.as_ptr().write_bytes(byte, scan.line_bytes) };
    }

    let mut flat = true;
    let mut used = 0;
    scan.act_index = [ACT_HOLD; 256];
    for b1 in 0..256 {
        if guard.group[b1] == 0 || core::ptr::eq(scan.rowset_tab[b1], &wf::HOLD) {
            continue;
        }
        if used == ACT_POOL {
            flat = false;
            break;
        }
        wf::build_act256(scan.rowset_tab[b1], &mut scan.act_pool[used]);
        scan.act_index[b1] = used;
        used += 1;
    }
    drop(guard);

    let pool = &*scan.act_pool;
    let tabs: [&[u8; 256]; 256] = core::array::from_fn(|b1| &pool[scan.act_index[b1]]);
    let bus = &scan.bus;
    bus.frame_begin();
    for y in 0..h {
        let nonidle = usize::from(shared.row_nonidle[y].load(Ordering::Relaxed));
        let line = if nonidle == 0 {
            &scan.hold_line
        } else if uniform.is_some() && nonidle == w {
            &scan.uni_line
        } else {
            let line = &scan.lines[y & 1];
            let dst = unsafe { slice::from_raw_parts_mut(line.as_ptr(), scan.line_bytes) };
            if flat {
                wf::blit_line_tab(shared.row(y), &tabs, dst);
            } else {
                wf::blit_line(shared.row(y), &scan.rowset_tab, dst);
            }
            line
        };
        bus.write_line(line);
    }
    bus.frame_end();

    let mut freed_total = 0;
    if any_retire {
        let mut guard = shared.lock.lock();
        for y in 0..h {
            if shared.row_nonidle[y].load(Ordering::Relaxed) == 0 {
                continue;
            }
            let freed = wf::retire_row(shared.row(y), &scan.retire_tab);
            sub(&shared.row_nonidle[y], freed);
            freed_total += u32::from(freed);
        }
        for b1 in 0..256 {
            if scan.retire_tab[b1] {
                guard.group[b1] = 0;
            }
        }
        guard.active_px -= freed_total;
    }
    if freed_total != 0 || shared.waiters.load(Ordering::SeqCst) > 0 {
        shared.retired.give();
    }
}

/// Sleeps while nothing replays, and powers the panel while something does.
fn refresh_task(shared: Arc<Shared>, mut scan: Scan) {
    let mut powered = false;
    loop {
        let active = {
            let mut guard = shared.lock.lock();
            if guard.active_px == 0 {
                guard.running = false;
            }
            guard.active_px != 0
        };
        if !active {
            if powered {
                scan.bus.power_off();
                powered = false;
            }
            shared.wake.take();
            continue;
        }
        if !powered {
            scan.bus.power_on();
            powered = true;
        }
        run_frame(&shared, &mut scan);
    }
}

/// The glass starts as it was, taken as white: clear or seed it before
/// trusting what a refresh drives.
pub(crate) struct EpdLl {
    shared: Arc<Shared>,
    size: Size,
    ppi: u16,
}

impl EpdLl {
    pub(crate) fn new(config: Config) -> Result<Self> {
        let Config { size, line_bytes, ref waveforms, .. } = config;
        let (width, height) = (usize::from(size.width), usize::from(size.height));
        if width % 4 != 0 || usize::from(line_bytes) < width / 4 {
            return Err(Error::INVALID_ARG);
        }
        let mut slots = [None; SLOTS];
        let tables = [waveforms.fast, waveforms.quality, waveforms.text, Some(waveforms.clear)];
        for (slot, table) in slots.iter_mut().zip(tables) {
            if let Some(table) = table {
                *slot = Some(Waveform::new(table).ok_or(Error::INVALID_ARG)?);
            }
        }

        let mut state = heap::try_vec(width * height, Caps::SPIRAM)?;
        state.extend((0..width * height).map(|_| AtomicU16::new(0xFFFF)));
        let mut row_nonidle = heap::try_vec(height, Caps::SPIRAM)?;
        row_nonidle.extend((0..height).map(|_| AtomicU16::new(0)));

        let bus = Bus::new_off(&config).inspect_err(|err| error!(TAG, "bus: {err}"))?;
        bus.power_off();
        let tx_len = bus.tx_len;
        let line = || bus.io.alloc_buffer(tx_len, Caps::DMA | Caps::INTERNAL);
        let (lines, hold_line, uni_line) = ([line()?, line()?], line()?, line()?);
        let act_pool = heap::zeroed_box::<[[u8; 256]; ACT_POOL + 1]>(Caps::INTERNAL)?;
        let scan = Scan {
            bus,
            rowset_tab: [&wf::HOLD; 256],
            retire_tab: [false; 256],
            act_index: [ACT_HOLD; 256],
            act_pool,
            lines,
            hold_line,
            uni_line,
            line_bytes: usize::from(line_bytes),
        };

        let shared = Arc::new(Shared {
            width,
            height,
            wf: slots,
            state: state.into_boxed_slice(),
            row_nonidle: row_nonidle.into_boxed_slice(),
            lock: Mutex::new(Counters { group: [0; 256], active_px: 0, frame: 0, running: false, mode: EpdMode::Hold }),
            waiters: AtomicU32::new(0),
            retired: Semaphore::try_new(0, 1)?,
            wake: Semaphore::try_new(0, 1)?,
        });
        let task = shared.clone();
        thread::Builder::new()
            .name("epd_refresh")
            .stack_size(TASK_STACK)
            .priority(config.priority)
            .core(config.core)
            .spawn(move || refresh_task(task, scan))
            .inspect_err(|err| error!(TAG, "task: {err}"))?;
        Ok(Self { shared, size, ppi: config.ppi })
    }
}

impl Driver for EpdLl {
    type FrameBuffers = Absent;
    type Gram = Self;
    type Epd = Self;

    fn size(&self) -> Size {
        self.size
    }

    fn ppi(&self) -> u16 {
        self.ppi
    }

    fn pixel_format(&self) -> ImageFormat {
        ImageFormat::L8
    }

    fn gram(&self) -> Option<&Self> {
        Some(self)
    }

    fn epd(&self) -> Option<&Self> {
        Some(self)
    }

    fn draw(&self, area: Rect, image: &Image) -> Result<()> {
        self.shared.draw(area, image.pixels(), image.stride_bytes())
    }

    fn reconfigure(&mut self, format: ImageFormat, rotation: Rotation) -> Result<()> {
        if format != ImageFormat::L8 || rotation != Rotation::R0 {
            return Err(Error::NOT_SUPPORTED);
        }
        Ok(())
    }
}

/// Pixels are taken in before `write` returns, which may wait for pixels of
/// the area still replaying.
impl GramDriver for EpdLl {
    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        self.shared.draw(area, pixels, area.w as usize)?;
        Ok(Ticket(0))
    }

    fn wait(&self, _ticket: Ticket) -> Result<()> {
        Ok(())
    }
}

impl EpdDriver for EpdLl {
    fn set_mode(&self, mode: EpdMode) -> Result<()> {
        self.shared.lock.lock().mode = mode;
        Ok(())
    }

    fn refresh(&self, area: Rect, waveform: display::Waveform, all: bool) -> Result<()> {
        self.shared.refresh(area, waveform, all)
    }

    fn clear(&self) -> Result<()> {
        self.shared.clear();
        Ok(())
    }

    fn wait_idle(&self) -> Result<()> {
        self.shared.wait_idle();
        Ok(())
    }
}
