/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! The i80 bus, the board's `Control` and the task replaying the waveforms.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::{CStr, c_void};
use core::slice;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

use devkit_core::boxed;
use idf::heap::{self, Caps};
use idf::lcd::i80::{self, DmaBuffer, Io};
use idf::log::error;
use idf::sync::{Mutex, MutexGuard, Semaphore};
use idf::time::delay_us;
use idf::{Error, Result, gpio, sys, system, thread};

use super::waveform::{self as wf, B1_IDLE, B1_PENDING, DC_ZERO, Frame, Pixel, Retire, Rowset, SLOTS, Waveform};
use crate::Absent;
use crate::display::{self, Driver, EpdDriver, EpdMode, GramDriver, Image, ImageFormat, Rect, Rotation, Size, Ticket};

const TAG: &CStr = c"epd_ll";

const SLOT_QUALITY: usize = 1;
const SLOT_CLEAR: usize = 3;
const QUEUE_DEPTH: usize = 4;
const DMA_BURST: usize = 64;
/// Most distinct frames one frame flattens; beyond it the scan reads the
/// waveforms themselves.
const ACT_POOL: usize = 16;
const ACT_HOLD: usize = ACT_POOL;
const TASK_STACK: usize = 6144;

/// The gate driver, the source latch and the power, as a board wires them.
pub(crate) trait Control: Send + Sync {
    /// Leaves everything off.
    fn init(&self) -> Result<()>;
    fn power_on(&self);
    fn power_off(&self);
    /// Scans the gate driver back to the top row.
    fn frame_begin(&self);
    /// After the last line has gone out.
    fn frame_end(&self);
    /// Before a line goes out.
    fn line_begin(&self);
    /// Once a line has gone out, from the bus interrupt.
    fn line_latch(&self);
}

/// CKV, SPV, LE, OE and the power rail each on a GPIO.
#[derive(Clone, Copy)]
pub(crate) struct ControlPins {
    pub ckv: i32,
    pub spv: i32,
    pub le: i32,
    pub oe: i32,
    pub pwr: i32,
}

impl Control for ControlPins {
    fn init(&self) -> Result<()> {
        let pins = [self.ckv, self.spv, self.le, self.oe, self.pwr];
        gpio::configure(&pins, gpio::Mode::Output)?;
        for pin in pins {
            level(pin, false);
        }
        Ok(())
    }

    fn power_on(&self) {
        level(self.oe, true);
        delay_us(100);
        level(self.pwr, true);
        delay_us(100);
    }

    fn power_off(&self) {
        level(self.pwr, false);
        delay_us(1000);
        level(self.oe, false);
        delay_us(100);
    }

    fn frame_begin(&self) {
        level(self.spv, false);
        delay_us(1);
        level(self.ckv, false);
        delay_us(3);
        level(self.ckv, true);
        delay_us(1);
        level(self.spv, true);
        for _ in 0..3 {
            delay_us(3);
            level(self.ckv, false);
            delay_us(3);
            level(self.ckv, true);
        }
    }

    fn frame_end(&self) {
        level(self.le, false);
        level(self.ckv, true);
    }

    fn line_begin(&self) {
        level(self.le, false);
        level(self.ckv, true);
    }

    fn line_latch(&self) {
        level(self.ckv, false);
        level(self.le, true);
    }
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
    /// The bus takes a D/C pin the panel lacks: a spare one, or one that
    /// `control` sets again in `power_off`.
    pub dc_dummy: i32,
    pub control: Box<dyn Control>,
    pub pclk_hz: u32,
    pub size: Size,
    /// L4 or L8.
    pub format: ImageFormat,
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
    control: Box<dyn Control>,
    busy: AtomicBool,
}

// The bus interrupt is not allocated IRAM-safe, so this may stay in flash.
unsafe extern "C" fn line_sent(_io: sys::esp_lcd_panel_io_handle_t, _data: *mut sys::esp_lcd_panel_io_event_data_t, ctx: *mut c_void) -> bool {
    let latch = unsafe { &*ctx.cast::<Latch>() };
    latch.control.line_latch();
    latch.busy.store(false, Ordering::Release);
    false
}

/// What the bus is built from.
struct Wiring {
    data: [i32; 8],
    sph: i32,
    cl: i32,
    dc_dummy: i32,
    pclk_hz: u32,
    tx_len: usize,
}

struct Bus {
    io: Io,
    latch: Box<Latch>,
    tx_len: usize,
}

impl Bus {
    fn new(wiring: &Wiring, latch: Box<Latch>) -> Result<Self> {
        latch.control.init()?;
        for &pin in wiring.data.iter().chain(&[wiring.cl, wiring.sph]) {
            gpio::set_direction(pin, gpio::Mode::Output)?;
        }
        let bus = i80::Bus::new(&i80::BusConfig {
            dc: wiring.dc_dummy,
            wr: wiring.cl,
            data: &wiring.data,
            max_transfer_bytes: wiring.tx_len,
            dma_burst_size: DMA_BURST,
        })?;
        // Back to a plain output; resetting the pin would pull it up for a
        // moment, a glitch on a power rail.
        gpio::set_direction(wiring.dc_dummy, gpio::Mode::Output)?;
        let ctx = (&*latch as *const Latch).cast_mut().cast();
        let io = unsafe {
            bus.new_io(
                &i80::IoConfig {
                    cs: wiring.sph,
                    pclk_hz: wiring.pclk_hz,
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
        Ok(Self { io, latch, tx_len: wiring.tx_len })
    }

    /// The bus interrupt lands on the core that creates the bus, and runs
    /// inline in `tx_color` there: on the other core it overlaps the next
    /// line's packing instead.
    fn new_off(wiring: &Wiring, latch: Box<Latch>, core: Option<u32>) -> Result<Self> {
        let cores = system::CORE_COUNT;
        let core = match core {
            Some(core) if core < cores => (core + 1) % cores,
            _ => (system::core_id() + 1) % cores,
        };
        thread::scope(|scope| {
            thread::Builder::new().name("epd_bus").stack_size(4096).core(Some(core)).spawn_scoped(scope, || Self::new(wiring, latch)).map(|h| h.join())
        })?
    }

    fn control(&self) -> &dyn Control {
        &*self.latch.control
    }

    fn wait(&self) {
        while self.latch.busy.load(Ordering::Acquire) {
            thread::yield_now();
        }
    }

    fn frame_begin(&self) {
        self.wait();
        self.control().frame_begin();
    }

    /// Waits for the line before, which the bus interrupt latched; `line`
    /// stays in use until the next call.
    fn write_line(&self, line: &DmaBuffer) {
        self.wait();
        self.latch.busy.store(true, Ordering::Release);
        self.control().line_begin();
        let data = unsafe { slice::from_raw_parts(line.as_ptr(), self.tx_len) };
        if let Err(err) = unsafe { self.io.tx_color(-1, data) } {
            error!(TAG, "line: {err}");
            self.latch.busy.store(false, Ordering::Release);
        }
    }

    fn frame_end(&self) {
        self.wait();
        self.control().frame_end();
    }
}

/// Written under `Shared::lock`.
struct Counters {
    /// Pixels per `b1`, idle ones aside.
    group: Box<[u32; 256], Caps>,
    active_px: u32,
    /// Mod 64.
    frame: u8,
    /// The task is driving rather than waiting on `wake`.
    running: bool,
    mode: EpdMode,
    /// Draws waiting for the scan to stop between frames.
    cutters: u32,
    wf: [Option<Waveform>; SLOTS],
    /// The table of the slot was a `Box` given to `set_waveform`, leaked
    /// while in use.
    owned: [bool; SLOTS],
}

struct Shared {
    width: usize,
    height: usize,
    /// Written under `lock`; the scan reads it without.
    state: Box<[Pixel], Caps>,
    /// Pixels per row that are not idle; written under `lock`.
    row_nonidle: Box<[AtomicU16], Caps>,
    lock: Mutex<Counters>,
    waiters: AtomicU32,
    /// Given every frame something finishes or someone waits.
    retired: Semaphore,
    wake: Semaphore,
    /// Given by the scan between frames to one of `cutters`, who gives
    /// `resumed` once done.
    boundary: Semaphore,
    resumed: Semaphore,
}

/// Pixels a draw left replaying toward another gray.
struct Pass {
    /// Some may be stopped at the next boundary.
    cuttable: bool,
    /// Some have a STOP frame ahead and must finish.
    stuck: bool,
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

    /// `pixels` are rows of `format`, L4 or L8, `stride` bytes apart.
    fn draw(&self, area: Rect, pixels: &[u8], stride: usize, format: ImageFormat) -> Result<()> {
        let row_bytes = format.row_bytes(area.w as usize);
        match format {
            ImageFormat::L4 => self.draw_grays(area, |r| wf::grays_l4(&pixels[r * stride..][..row_bytes])),
            _ => self.draw_grays(area, |r| wf::grays_l8(&pixels[r * stride..][..row_bytes])),
        }
    }

    /// `grays(r)` gives row `r` of `area`.
    fn draw_grays<I: Iterator<Item = u8>>(&self, area: Rect, grays: impl Fn(usize) -> I) -> Result<()> {
        let (x0, y0, w, h) = (area.x as usize, area.y as usize, area.w as usize, area.h as usize);
        let mut guard = self.lock.lock();
        let mode = guard.mode;
        if mode == EpdMode::Seed {
            while self.area_has_active(x0, y0, x0 + w, y0 + h) {
                guard = self.wait_retire(guard);
            }
            let Counters { group, wf: waveforms, .. } = &mut *guard;
            let end = waveforms[SLOT_QUALITY].as_ref().map_or(&wf::KEEP, |q| q.dc_after(q.steps()));
            for r in 0..h {
                let mut subsumed = 0;
                for (px, g) in self.row(y0 + r)[x0..x0 + w].iter().zip(grays(r)) {
                    let v = px.load(Ordering::Relaxed);
                    subsumed += u16::from((v >> 8) as u8 == B1_PENDING);
                    px.store(wf::seed_px(v, g, end), Ordering::Relaxed);
                }
                sub(&self.row_nonidle[y0 + r], subsumed);
                group[usize::from(B1_PENDING)] -= u32::from(subsumed);
            }
            return Ok(());
        }
        let mut pass = self.draw_pass(&mut guard, area, &grays, false);
        loop {
            if pass.cuttable {
                guard = self.at_boundary(guard);
                pass = self.draw_pass(&mut guard, area, &grays, true);
                guard.cutters -= 1;
                drop(guard);
                self.resumed.give();
                guard = self.lock.lock();
            }
            if !pass.stuck {
                break;
            }
            // Holding the scan at every boundary would slow the replays the
            // draw waits for.
            guard = self.wait_retire(guard);
            pass = self.draw_pass(&mut guard, area, &grays, false);
        }
        drop(guard);
        match mode {
            EpdMode::Refresh(waveform) => self.refresh(area, waveform, false),
            _ => Ok(()),
        }
    }

    /// Draws `area`, leaving pixels replaying toward another gray as they
    /// were. With `cut`, which only a draw between frames may ask, those a
    /// draw may stop stop where the steps they have run leave them instead.
    fn draw_pass<I: Iterator<Item = u8>>(&self, guard: &mut Counters, area: Rect, grays: &impl Fn(usize) -> I, cut: bool) -> Pass {
        let (x0, y0, w, h) = (area.x as usize, area.y as usize, area.w as usize, area.h as usize);
        let Counters { group, wf: waveforms, frame, active_px, .. } = guard;
        let mut pass = Pass { cuttable: false, stuck: false };
        for r in 0..h {
            let mut gained = 0;
            for (px, g) in self.row(y0 + r)[x0..x0 + w].iter().zip(grays(r)) {
                let v = px.load(Ordering::Relaxed);
                let drawn = wf::draw_px(v, g);
                if drawn.conflict {
                    let b1 = (v >> 8) as u8;
                    match wf::cut_dc(waveforms[usize::from(b1 >> 6)].as_ref(), *frame, b1) {
                        Some(dc) if cut => {
                            px.store(wf::cut_px(v, g, dc), Ordering::Relaxed);
                            group[usize::from(b1)] -= 1;
                            group[usize::from(B1_PENDING)] += 1;
                            *active_px -= 1;
                        }
                        Some(_) => pass.cuttable = true,
                        None => pass.stuck = true,
                    }
                    continue;
                }
                px.store(drawn.px, Ordering::Relaxed);
                gained += u16::from(drawn.now_nonidle);
            }
            add(&self.row_nonidle[y0 + r], gained);
            group[usize::from(B1_PENDING)] += u32::from(gained);
        }
        pass
    }

    /// Waits for the scan to finish the frame it is sending and holds `lock`
    /// again before the next one starts. `cutters` stays raised until the
    /// caller lowers it and gives `resumed`.
    fn at_boundary<'a>(&'a self, mut guard: MutexGuard<'a, Counters>) -> MutexGuard<'a, Counters> {
        guard.cutters += 1;
        drop(guard);
        self.boundary.take();
        self.lock.lock()
    }

    fn refresh(&self, area: Rect, waveform: display::Waveform, all: bool) -> Result<()> {
        let slot = match waveform {
            display::Waveform::Fast => 0,
            display::Waveform::Quality => 1,
            display::Waveform::Text => 2,
        };
        let (x0, y0) = (area.x as usize, area.y as usize);
        let (x1, y1) = (x0 + area.w as usize, y0 + area.h as usize);
        let mut guard = self.lock.lock();
        if guard.wf[slot].is_none() {
            return Ok(());
        }
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
    rowset_tab: [Rowset; 256],
    retire_tab: [Retire; 256],
    act_index: [usize; 256],
    /// Internal RAM, so the scan never reaches the waveforms in flash; the
    /// last one stays all hold.
    act_pool: Box<[[u8; 256]; ACT_POOL + 1], Caps>,
    lines: [DmaBuffer; 2],
    hold_line: DmaBuffer,
    uni_line: DmaBuffer,
    line_bytes: usize,
}

// `rowset_tab` and `retire_tab` point into tables that are only replaced
// while nothing replays, between frames.
unsafe impl Send for Scan {}

fn run_frame(shared: &Shared, scan: &mut Scan) {
    let (w, h) = (shared.width, shared.height);
    let mut guard = shared.lock.lock();
    guard.frame = (guard.frame + 1) & 63;
    let skip_settle = shared.waiters.load(Ordering::SeqCst) > 0;
    wf::build_frame_tab(&guard.wf, guard.frame, skip_settle, &mut scan.rowset_tab, &mut scan.retire_tab);

    // Whether anything finishes, and whether one frame that drives every
    // pixel alike is all that replays, so one line serves every full row.
    let (mut any_retire, mut multiple) = (false, false);
    let mut only: Option<Rowset> = None;
    for b1 in 0..256 {
        if guard.group[b1] == 0 {
            continue;
        }
        any_retire |= !scan.retire_tab[b1].is_null();
        match only {
            None => only = Some(scan.rowset_tab[b1]),
            Some(f) => multiple |= !core::ptr::eq(f, scan.rowset_tab[b1]),
        }
    }
    let uniform = match only {
        Some(f) if !multiple && guard.group[usize::from(B1_PENDING)] == 0 => wf::rowset_uniform(unsafe { &*f }),
        _ => None,
    };
    if let Some(byte) = uniform {
        unsafe { scan.uni_line.as_ptr().write_bytes(byte, scan.line_bytes) };
    }

    let mut flat = true;
    let mut used = 0;
    scan.act_index = [ACT_HOLD; 256];
    for b1 in 0..256 {
        if guard.group[b1] == 0 || core::ptr::eq(scan.rowset_tab[b1], wf::hold()) {
            continue;
        }
        if used == ACT_POOL {
            flat = false;
            break;
        }
        wf::build_act256(unsafe { &*scan.rowset_tab[b1] }, &mut scan.act_pool[used]);
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
                unsafe { wf::blit_line(shared.row(y), &scan.rowset_tab, dst) };
            }
            line
        };
        bus.write_line(line);
    }
    bus.frame_end();

    let mut freed_total = 0;
    let mut guard = shared.lock.lock();
    if any_retire {
        for y in 0..h {
            if shared.row_nonidle[y].load(Ordering::Relaxed) == 0 {
                continue;
            }
            let freed = unsafe { wf::retire_row(shared.row(y), &scan.retire_tab) };
            sub(&shared.row_nonidle[y], freed);
            freed_total += u32::from(freed);
        }
        for b1 in 0..256 {
            if !scan.retire_tab[b1].is_null() {
                guard.group[b1] = 0;
            }
        }
        guard.active_px -= freed_total;
    }
    while guard.cutters > 0 {
        drop(guard);
        shared.boundary.give();
        shared.resumed.take();
        guard = shared.lock.lock();
    }
    drop(guard);
    if freed_total != 0 || shared.waiters.load(Ordering::SeqCst) > 0 {
        shared.retired.give();
    }
}

/// Sleeps while nothing replays, and powers the panel while something does.
fn refresh_task(shared: Arc<Shared>, mut scan: Box<Scan>) {
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
                scan.bus.control().power_off();
                powered = false;
            }
            shared.wake.take();
            continue;
        }
        if !powered {
            scan.bus.control().power_on();
            powered = true;
        }
        run_frame(&shared, &mut scan);
    }
}

/// The glass starts as it was, taken as white and without DC: clear or seed
/// it before trusting what a refresh drives.
pub(crate) struct EpdLl {
    shared: Arc<Shared>,
    size: Size,
    ppi: u16,
    format: ImageFormat,
}

fn takes(format: ImageFormat) -> bool {
    matches!(format, ImageFormat::L4 | ImageFormat::L8)
}

impl EpdLl {
    pub(crate) fn new(config: Config) -> Result<Self> {
        let Config { size, line_bytes, ref waveforms, .. } = config;
        let (width, height) = (usize::from(size.width), usize::from(size.height));
        if width % 4 != 0 || usize::from(line_bytes) < width / 4 || !takes(config.format) {
            return Err(Error::INVALID_ARG);
        }
        let mut slots = [const { None }; SLOTS];
        let tables = [waveforms.fast, waveforms.quality, waveforms.text, Some(waveforms.clear)];
        for (slot, table) in slots.iter_mut().zip(tables) {
            if let Some(table) = table {
                *slot = Some(Waveform::new(table).inspect_err(|err| error!(TAG, "waveform: {err}"))?);
            }
        }

        let mut state = heap::try_vec(width * height, Caps::SPIRAM)?;
        state.extend((0..width * height).map(|_| AtomicU16::new(0xFF00 | DC_ZERO << 4 | 15)));
        let mut row_nonidle = heap::try_vec(height, Caps::SPIRAM)?;
        row_nonidle.extend((0..height).map(|_| AtomicU16::new(0)));

        let wiring = Wiring {
            data: config.data,
            sph: config.sph,
            cl: config.cl,
            dc_dummy: config.dc_dummy,
            pclk_hz: config.pclk_hz,
            tx_len: usize::from(line_bytes) + usize::from(config.line_padding),
        };
        let latch = Box::new(Latch { control: config.control, busy: AtomicBool::new(false) });
        let bus = Bus::new_off(&wiring, latch, config.core).inspect_err(|err| error!(TAG, "bus: {err}"))?;
        bus.control().power_off();
        let tx_len = bus.tx_len;
        let line = || bus.io.alloc_buffer(tx_len, Caps::DMA | Caps::INTERNAL);
        let (lines, hold_line, uni_line) = ([line()?, line()?], line()?, line()?);
        let act_pool = heap::zeroed_box::<[[u8; 256]; ACT_POOL + 1]>(Caps::INTERNAL)?;
        // Straight into the heap: on the stack it would take more than a
        // default main task has.
        let scan = boxed!(Scan {
            bus,
            rowset_tab: [wf::hold(); 256],
            retire_tab: [core::ptr::null(); 256],
            act_index: [ACT_HOLD; 256],
            act_pool,
            lines,
            hold_line,
            uni_line,
            line_bytes: usize::from(line_bytes),
        });

        let shared = Arc::new(Shared {
            width,
            height,
            state: state.into_boxed_slice(),
            row_nonidle: row_nonidle.into_boxed_slice(),
            lock: Mutex::new(Counters {
                group: heap::zeroed_box(Caps::INTERNAL)?,
                active_px: 0,
                frame: 0,
                running: false,
                mode: EpdMode::Hold,
                cutters: 0,
                wf: slots,
                owned: [false; SLOTS],
            }),
            waiters: AtomicU32::new(0),
            retired: Semaphore::try_new(0, 1)?,
            wake: Semaphore::try_new(0, 1)?,
            boundary: Semaphore::try_new(0, 1)?,
            resumed: Semaphore::try_new(0, 1)?,
        });
        let task = shared.clone();
        thread::Builder::new()
            .name("epd_refresh")
            .stack_size(TASK_STACK)
            .priority(config.priority)
            .core(config.core)
            .spawn(move || refresh_task(task, scan))
            .inspect_err(|err| error!(TAG, "task: {err}"))?;
        Ok(Self { shared, size, ppi: config.ppi, format: config.format })
    }

    /// Waits until nothing replays, then binds `lut` to `slot` and returns the
    /// table it replaces when that one was given here too.
    pub(crate) fn set_waveform(&self, slot: usize, lut: Box<[Frame]>) -> Result<Option<Box<[Frame]>>> {
        if slot >= SLOTS || !wf::check(&lut) {
            return Err(Error::INVALID_ARG);
        }
        let lut: &'static [Frame] = Box::leak(lut);
        let waveform = Waveform::new(lut).inspect_err(|_| drop(unsafe { Box::from_raw(core::ptr::from_ref(lut).cast_mut()) }))?;
        let mut guard = self.shared.lock.lock();
        while guard.active_px != 0 {
            guard = self.shared.wait_retire(guard);
        }
        let old = guard.wf[slot].replace(waveform);
        let owned = core::mem::replace(&mut guard.owned[slot], true);
        drop(guard);
        Ok(old.filter(|_| owned).map(|old| unsafe { Box::from_raw(core::ptr::from_ref(old.lut()).cast_mut()) }))
    }

    /// A copy of the table bound to `slot`.
    pub(crate) fn waveform(&self, slot: usize) -> Option<Box<[Frame]>> {
        let guard = self.shared.lock.lock();
        guard.wf.get(slot)?.as_ref().map(|w| Box::from(w.lut()))
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
        self.format
    }

    fn alignment(&self) -> u16 {
        if self.format == ImageFormat::L4 { 2 } else { 1 }
    }

    fn gram(&self) -> Option<&Self> {
        Some(self)
    }

    fn epd(&self) -> Option<&Self> {
        Some(self)
    }

    fn draw(&self, area: Rect, image: &Image) -> Result<()> {
        self.shared.draw(area, image.pixels(), image.stride_bytes(), self.format)
    }

    fn reconfigure(&mut self, format: ImageFormat, rotation: Rotation) -> Result<()> {
        if !takes(format) || rotation != Rotation::R0 {
            return Err(Error::NOT_SUPPORTED);
        }
        self.format = format;
        Ok(())
    }
}

/// Pixels are taken in before `write` returns, which may wait for pixels of
/// the area still replaying.
impl GramDriver for EpdLl {
    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        self.shared.draw(area, pixels, self.format.row_bytes(area.w as usize), self.format)?;
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
