/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! The i80 bus, the board's `Control` and the task replaying the waveforms.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{CStr, c_void};
use core::slice;
use core::sync::atomic::{AtomicU16, AtomicU32, Ordering};

use devkit_core::boxed;
use idf::heap::{self, Caps};
use idf::lcd::i80::{self, DmaBuffer, Io};
use idf::log::error;
use idf::sync::{Mutex, MutexGuard, Receiver, Semaphore, Sender, channel};
use idf::time::delay_us;
use idf::{Error, Result, gpio, sys, system, thread};

use super::waveform::{self as wf, B1_IDLE, B1_PENDING, DC_ZERO, Frame, Next, Pixel, Retire, Rowset, SLOTS, Waveform};
use crate::Absent;
use crate::display::{self, Driver, EpdDriver, EpdMode, GramDriver, Image, ImageFormat, Rect, RefreshUnit, Rotation, Size, Ticket};

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
const WRITE_STACK: usize = 4096;
/// Writes, refreshes and the like a writer may have queued.
const JOB_DEPTH: usize = 16;
const MAX_WAKES: u32 = 8;
const MAX_DEFERRED: usize = 8;

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
    /// Held while a line is on the bus; the bus interrupt gives it back.
    free: Semaphore,
}

// The bus interrupt is not allocated IRAM-safe, so this may stay in flash.
unsafe extern "C" fn line_sent(_io: sys::esp_lcd_panel_io_handle_t, _data: *mut sys::esp_lcd_panel_io_event_data_t, ctx: *mut c_void) -> bool {
    let latch = unsafe { &*ctx.cast::<Latch>() };
    latch.control.line_latch();
    latch.free.give_from_isr();
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

    /// Blocks, rather than spinning, so the core stays with other tasks
    /// whatever this one's priority.
    fn wait(&self) {
        self.latch.free.take();
        self.latch.free.give();
    }

    fn frame_begin(&self) {
        self.wait();
        self.control().frame_begin();
    }

    /// Waits for the line before, which the bus interrupt latched; `line`
    /// stays in use until the next call.
    fn write_line(&self, line: &DmaBuffer) {
        self.latch.free.take();
        self.control().line_begin();
        let data = unsafe { slice::from_raw_parts(line.as_ptr(), self.tx_len) };
        if let Err(err) = unsafe { self.io.tx_color(-1, data) } {
            error!(TAG, "line: {err}");
            self.latch.free.give();
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
    /// Pixels with a gray to go to after their replay.
    waiting_px: u32,
    /// Refreshes, by area and slot, of pixels that were replaying, done again
    /// between frames once those are drawn.
    deferred: Vec<(Rect, usize)>,
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
    /// Under `lock`.
    next: Box<[Next], Caps>,
    /// Pixels per row with a gray to go to after their replay; under `lock`.
    row_waiting: Box<[AtomicU16], Caps>,
    lock: Mutex<Counters>,
    waiters: AtomicU32,
    /// Given every frame something finishes or someone waits.
    retired: Semaphore,
    wake: Semaphore,
}

fn slot_of(waveform: display::Waveform) -> usize {
    match waveform {
        display::Waveform::Fast => 0,
        display::Waveform::Quality => 1,
        display::Waveform::Text => 2,
    }
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
                let y = y0 + r;
                let mut subsumed = 0;
                for (x, g) in (x0..x0 + w).zip(grays(r)) {
                    let i = y * self.width + x;
                    let v = self.state[i].load(Ordering::Relaxed);
                    subsumed += u16::from((v >> 8) as u8 == B1_PENDING);
                    self.state[i].store(wf::seed_px(v, g, end), Ordering::Relaxed);
                }
                sub(&self.row_nonidle[y], subsumed);
                group[usize::from(B1_PENDING)] -= u32::from(subsumed);
            }
            return Ok(());
        }
        drop(guard);
        let (slot, all) = match mode {
            EpdMode::Refresh(waveform) => (Some(slot_of(waveform)), false),
            EpdMode::RefreshAll(waveform) => (Some(slot_of(waveform)), true),
            _ => (None, false),
        };
        let mut wake = false;
        // A row at a time, so the scan never waits for a whole area.
        for r in 0..h {
            let y = y0 + r;
            let start = y * self.width + x0;
            let (row, next) = (&self.state[start..][..w], &self.next[start..][..w]);
            let mut guard = self.lock.lock();
            let counters = &mut *guard;
            let slot = slot.filter(|&s| counters.wf[s].is_some());
            let b1 = slot.map(|s| wf::armed(s, counters.frame));
            let (mut gained, mut armed, mut pending, mut waiting) = (0u16, 0u32, 0i32, 0i32);
            for ((px, n), g) in row.iter().zip(next).zip(grays(r)) {
                let v = px.load(Ordering::Relaxed);
                if wf::is_active(v) {
                    let target = (v & 15) as u8;
                    let (old, new) = (n.load(Ordering::Relaxed), wf::next_byte(g, slot));
                    if old != new {
                        n.store(new, Ordering::Relaxed);
                        waiting += i32::from(g != target) - i32::from(wf::next_gray(old) != target);
                    }
                    continue;
                }
                let d = wf::draw_px(v, g, b1, all);
                if d == v {
                    continue;
                }
                px.store(d, Ordering::Relaxed);
                let (was, now) = ((v >> 8) as u8, (d >> 8) as u8);
                gained += u16::from(was == B1_IDLE);
                pending += i32::from(now == B1_PENDING) - i32::from(was == B1_PENDING);
                if now < B1_PENDING {
                    n.store(g, Ordering::Relaxed);
                    armed += 1;
                }
            }
            add(&self.row_nonidle[y], gained);
            let p = &mut counters.group[usize::from(B1_PENDING)];
            *p = p.wrapping_add_signed(pending);
            if waiting != 0 {
                add(&self.row_waiting[y], waiting as u16);
                counters.waiting_px = counters.waiting_px.wrapping_add_signed(waiting);
            }
            if let Some(b1) = b1.filter(|_| armed != 0) {
                counters.group[usize::from(b1)] += armed;
                counters.active_px += armed;
                wake |= self.start(counters);
            }
        }
        if wake {
            self.wake.give();
        }
        Ok(())
    }

    fn waiting_in(&self, area: Rect) -> bool {
        (area.y as usize..area.bottom() as usize).any(|y| self.row_waiting[y].load(Ordering::Relaxed) != 0)
    }

    /// Between frames, before the next is counted: replaying pixels a draw
    /// may stop now go to what was written for them. A row at a time, so
    /// draws go on meanwhile.
    fn settle_written(&self) {
        for y in 0..self.height {
            if self.row_waiting[y].load(Ordering::Relaxed) == 0 {
                continue;
            }
            let start = y * self.width;
            let (row, next) = (&self.state[start..][..self.width], &self.next[start..][..self.width]);
            let mut guard = self.lock.lock();
            let Counters { group, wf: waveforms, frame, active_px, waiting_px, .. } = &mut *guard;
            let (mut settled, mut rearmed) = (0u16, 0u32);
            for (px, cell) in row.iter().zip(next) {
                let v = px.load(Ordering::Relaxed);
                if !wf::is_active(v) {
                    continue;
                }
                let n = cell.load(Ordering::Relaxed);
                let gray = wf::next_gray(n);
                if u16::from(gray) == v & 15 {
                    continue;
                }
                let b1 = (v >> 8) as u8;
                let Some(dc) = wf::cut_dc(waveforms[usize::from(b1 >> 6)].as_ref(), *frame, b1) else { continue };
                let cut = wf::cut_px(v, gray, dc);
                group[usize::from(b1)] -= 1;
                settled += 1;
                match wf::next_slot(n).filter(|&s| waveforms[s].is_some()) {
                    Some(slot) => {
                        let armed = wf::armed(slot, *frame);
                        px.store((cut & 0x00FF) | u16::from(armed) << 8, Ordering::Relaxed);
                        cell.store(gray, Ordering::Relaxed);
                        group[usize::from(armed)] += 1;
                        rearmed += 1;
                    }
                    None => {
                        px.store(cut, Ordering::Relaxed);
                        group[usize::from(B1_PENDING)] += 1;
                    }
                }
            }
            *active_px -= u32::from(settled) - rearmed;
            sub(&self.row_waiting[y], settled);
            *waiting_px -= u32::from(settled);
        }
    }

    /// The refreshes pixels that were replaying missed, once those are drawn.
    fn arm_deferred(&self, counters: &mut Counters) {
        for (area, slot) in core::mem::take(&mut counters.deferred) {
            self.arm(counters, area, slot, false);
            if self.waiting_in(area) {
                counters.deferred.push((area, slot));
            }
        }
    }

    /// Arms the pending pixels of `area` with `slot`, and the idle ones too
    /// with `all`; those replaying go on. Whether the scan is to be woken.
    fn arm(&self, counters: &mut Counters, area: Rect, slot: usize, all: bool) -> bool {
        if counters.wf[slot].is_none() {
            return false;
        }
        let (x0, y0) = (area.x as usize, area.y as usize);
        let (x1, y1) = (x0 + area.w as usize, y0 + area.h as usize);
        let b1 = wf::armed(slot, counters.frame);
        let mut armed = 0;
        for y in y0..y1 {
            if !all && self.row_nonidle[y].load(Ordering::Relaxed) == 0 {
                continue;
            }
            let (mut row_armed, mut new_nonidle) = (0u32, 0u16);
            for x in x0..x1 {
                let i = y * self.width + x;
                let px = &self.state[i];
                let v = px.load(Ordering::Relaxed);
                let pb1 = (v >> 8) as u8;
                if pb1 == B1_PENDING {
                    counters.group[usize::from(B1_PENDING)] -= 1;
                } else if !(all && pb1 == B1_IDLE) {
                    continue;
                } else {
                    new_nonidle += 1;
                }
                px.store((v & 0x00FF) | u16::from(b1) << 8, Ordering::Relaxed);
                self.next[i].store((v & 15) as u8, Ordering::Relaxed);
                row_armed += 1;
            }
            add(&self.row_nonidle[y], new_nonidle);
            armed += row_armed;
        }
        if armed == 0 {
            return false;
        }
        counters.group[usize::from(b1)] += armed;
        counters.active_px += armed;
        self.start(counters)
    }

    /// `waveform::retire_row` for a row where some replaying pixels were
    /// written: those go on to what was written. How many finished, how many
    /// of them went idle, and how many replay again.
    fn retire_waiting_row(&self, counters: &mut Counters, y: usize, retire_tab: &[Retire; 256]) -> (u16, u16, u32) {
        let (mut retired, mut freed, mut pending, mut rearmed) = (0u16, 0u16, 0u32, 0u32);
        let start = y * self.width;
        let (row, next) = (&self.state[start..][..self.width], &self.next[start..][..self.width]);
        for (px, cell) in row.iter().zip(next) {
            let v = px.load(Ordering::Relaxed);
            let retire = retire_tab[usize::from(v >> 8)];
            if retire.is_null() {
                continue;
            }
            // `retire_tab` points at live tables, as for `waveform::retire_row`.
            let dc = u16::from(unsafe { &*retire }[usize::from(v & 0xFF)]);
            let n = cell.load(Ordering::Relaxed);
            let (target, gray) = (v & 15, u16::from(wf::next_gray(n)));
            retired += 1;
            if gray == target {
                px.store(0xFF00 | dc << 4 | target, Ordering::Relaxed);
                freed += 1;
                continue;
            }
            match wf::next_slot(n).filter(|&s| counters.wf[s].is_some()) {
                Some(slot) => {
                    let armed = wf::armed(slot, counters.frame);
                    px.store(u16::from(armed) << 8 | dc << 4 | gray, Ordering::Relaxed);
                    cell.store(gray as u8, Ordering::Relaxed);
                    counters.group[usize::from(armed)] += 1;
                    rearmed += 1;
                }
                None => {
                    px.store(u16::from(B1_PENDING) << 8 | dc << 4 | gray, Ordering::Relaxed);
                    pending += 1;
                }
            }
        }
        let settled = pending + rearmed;
        counters.group[usize::from(B1_PENDING)] += pending;
        sub(&self.row_waiting[y], settled as u16);
        counters.waiting_px -= settled;
        (retired, freed, rearmed)
    }

    /// `arm`, and again between frames for what replaying pixels were
    /// written.
    fn refresh_locked(&self, counters: &mut Counters, area: Rect, slot: usize, all: bool) -> bool {
        let wake = self.arm(counters, area, slot, all);
        if self.waiting_in(area) {
            let full = counters.deferred.len() >= MAX_DEFERRED;
            match counters.deferred.iter_mut().find(|(_, s)| *s == slot) {
                Some((kept, _)) if full => *kept = kept.union(&area),
                _ => counters.deferred.push((area, slot)),
            }
        }
        wake
    }

    fn refresh(&self, area: Rect, waveform: display::Waveform, all: bool) {
        let mut guard = self.lock.lock();
        let wake = self.refresh_locked(&mut guard, area, slot_of(waveform), all);
        drop(guard);
        if wake {
            self.wake.give();
        }
    }

    fn clear(&self, area: Rect) {
        let (x0, y0) = (area.x as usize, area.y as usize);
        let (x1, y1) = (x0 + area.w as usize, y0 + area.h as usize);
        let mut guard = self.lock.lock();
        // Armed at 61 or 62, its b1 would be a marker. With nothing replaying
        // the frame count is free to move; otherwise the frames move it on.
        loop {
            if guard.active_px == 0 && matches!(guard.frame, 61 | 62) {
                guard.frame = 63;
            }
            if !matches!(guard.frame, 61 | 62) && !self.area_has_active(x0, y0, x1, y1) {
                break;
            }
            guard = self.wait_retire(guard);
        }
        let b1 = wf::armed(SLOT_CLEAR, guard.frame);
        let mut armed = 0;
        let Counters { group, .. } = &mut *guard;
        for y in y0..y1 {
            let mut now_nonidle = 0u16;
            for x in x0..x1 {
                let i = y * self.width + x;
                let v = self.state[i].load(Ordering::Relaxed);
                match (v >> 8) as u8 {
                    B1_PENDING => group[usize::from(B1_PENDING)] -= 1,
                    _ => now_nonidle += 1,
                }
                self.state[i].store(u16::from(b1) << 8 | (v & 0x00F0) | 15, Ordering::Relaxed);
                self.next[i].store(15, Ordering::Relaxed);
            }
            add(&self.row_nonidle[y], now_nonidle);
            armed += (x1 - x0) as u32;
        }
        guard.group[usize::from(b1)] += armed;
        guard.active_px += armed;
        let wake = self.start(&mut guard);
        drop(guard);
        if wake {
            self.wake.give();
        }
    }

    /// Whether `area` holds a pixel a draw could not stop.
    fn busy(&self, area: Rect) -> bool {
        let (x0, y0) = (area.x as usize, area.y as usize);
        let (x1, y1) = (x0 + area.w as usize, y0 + area.h as usize);
        let guard = self.lock.lock();
        let stopping = (0..usize::from(B1_PENDING)).any(|b1| guard.group[b1] != 0 && guard.wf[b1 >> 6].as_ref().is_some_and(Waveform::stops));
        if !stopping {
            return false;
        }
        (y0..y1).any(|y| {
            self.row_nonidle[y].load(Ordering::Relaxed) != 0
                && self.row(y)[x0..x1].iter().any(|px| {
                    let v = px.load(Ordering::Relaxed);
                    let b1 = (v >> 8) as u8;
                    wf::is_active(v) && wf::cut_dc(guard.wf[usize::from(b1 >> 6)].as_ref(), guard.frame, b1).is_none()
                })
        })
    }

    fn wait_idle(&self) {
        let mut guard = self.lock.lock();
        while guard.active_px != 0 || guard.waiting_px != 0 {
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
    shared.settle_written();
    let mut guard = shared.lock.lock();
    shared.arm_deferred(&mut guard);
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

    let mut retired_total = 0;
    if any_retire {
        // A row at a time, so draws go on meanwhile; they arm nothing that
        // finishes now.
        for y in 0..h {
            if shared.row_nonidle[y].load(Ordering::Relaxed) == 0 {
                continue;
            }
            let mut guard = shared.lock.lock();
            let (retired, freed, rearmed) = if shared.row_waiting[y].load(Ordering::Relaxed) == 0 {
                let freed = unsafe { wf::retire_row(shared.row(y), &scan.retire_tab) };
                (freed, freed, 0)
            } else {
                shared.retire_waiting_row(&mut guard, y, &scan.retire_tab)
            };
            sub(&shared.row_nonidle[y], freed);
            guard.active_px -= u32::from(retired) - rearmed;
            retired_total += u32::from(retired);
        }
        let mut guard = shared.lock.lock();
        for b1 in 0..256 {
            if !scan.retire_tab[b1].is_null() {
                guard.group[b1] = 0;
            }
        }
    }
    if retired_total != 0 || shared.waiters.load(Ordering::SeqCst) > 0 {
        shared.retired.give();
    }
}

/// Sleeps while nothing replays, and powers the panel while something does.
fn refresh_task(shared: Arc<Shared>, mut scan: Box<Scan>) {
    let mut powered = false;
    loop {
        let active = {
            let mut guard = shared.lock.lock();
            let active = guard.active_px != 0 || guard.waiting_px != 0;
            if !active {
                guard.running = false;
            }
            active
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
    jobs: Sender<Job>,
    issued: Mutex<u32>,
    progress: Arc<Progress>,
}

/// What the write task does, in the order given.
enum Job {
    Write { area: Rect, pixels: *const u8, len: usize, stride: usize, format: ImageFormat },
    Refresh { area: Rect, waveform: display::Waveform, all: bool },
    Clear(Rect),
    Mode(EpdMode),
}

// A writer keeps its pixels until it has waited for the ticket.
unsafe impl Send for Job {}

/// How many jobs the write task has done.
struct Progress {
    done: AtomicU32,
    waiters: AtomicU32,
    wake: Semaphore,
}

impl Progress {
    fn reached(&self, ticket: Ticket) -> bool {
        self.done.load(Ordering::SeqCst).wrapping_sub(ticket.0) as i32 >= 0
    }

    fn wait(&self, ticket: Ticket) {
        while !self.reached(ticket) {
            self.waiters.fetch_add(1, Ordering::SeqCst);
            if !self.reached(ticket) {
                self.wake.take();
            }
            self.waiters.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn write_task(shared: Arc<Shared>, jobs: Receiver<Job>, progress: Arc<Progress>) {
    loop {
        match jobs.recv() {
            Job::Write { area, pixels, len, stride, format } => {
                let pixels = unsafe { slice::from_raw_parts(pixels, len) };
                if let Err(err) = shared.draw(area, pixels, stride, format) {
                    error!(TAG, "write {area:?}: {err}");
                }
            }
            Job::Refresh { area, waveform, all } => shared.refresh(area, waveform, all),
            Job::Clear(area) => shared.clear(area),
            Job::Mode(mode) => shared.lock.lock().mode = mode,
        }
        progress.done.fetch_add(1, Ordering::SeqCst);
        for _ in 0..progress.waiters.load(Ordering::SeqCst) {
            progress.wake.give();
        }
    }
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
        let mut next = heap::try_vec(width * height, Caps::SPIRAM)?;
        next.extend((0..width * height).map(|_| Next::new(15)));
        let mut row_waiting = heap::try_vec(height, Caps::SPIRAM)?;
        row_waiting.extend((0..height).map(|_| AtomicU16::new(0)));

        let wiring = Wiring {
            data: config.data,
            sph: config.sph,
            cl: config.cl,
            dc_dummy: config.dc_dummy,
            pclk_hz: config.pclk_hz,
            tx_len: usize::from(line_bytes) + usize::from(config.line_padding),
        };
        let latch = Box::new(Latch { control: config.control, free: Semaphore::try_new(1, 1)? });
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
            next: next.into_boxed_slice(),
            row_waiting: row_waiting.into_boxed_slice(),
            lock: Mutex::new(Counters {
                group: heap::zeroed_box(Caps::INTERNAL)?,
                active_px: 0,
                frame: 0,
                running: false,
                mode: EpdMode::Hold,
                waiting_px: 0,
                deferred: Vec::new(),
                wf: slots,
                owned: [false; SLOTS],
            }),
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
        let progress = Arc::new(Progress { done: AtomicU32::new(0), waiters: AtomicU32::new(0), wake: Semaphore::try_new(0, MAX_WAKES)? });
        let (jobs, receiver) = channel(JOB_DEPTH);
        let (task, done) = (shared.clone(), progress.clone());
        // Off the scan's core, which it keeps busy while it drives.
        let core = config.core.map(|core| (core + 1) % system::CORE_COUNT);
        thread::Builder::new()
            .name("epd_write")
            .stack_size(WRITE_STACK)
            .priority(config.priority)
            .core(core)
            .spawn(move || write_task(task, receiver, done))
            .inspect_err(|err| error!(TAG, "write task: {err}"))?;
        Ok(Self { shared, size, ppi: config.ppi, format: config.format, jobs, issued: Mutex::new(0), progress })
    }

    fn submit(&self, job: Job) -> Ticket {
        let mut issued = self.issued.lock();
        *issued = issued.wrapping_add(1);
        self.jobs.send(job);
        Ticket(*issued)
    }

    /// Waits until the write task has done everything given so far.
    fn drain(&self) {
        let last = Ticket(*self.issued.lock());
        self.progress.wait(last);
    }

    /// Waits until nothing replays, then binds `lut` to `slot` and returns the
    /// table it replaces when that one was given here too.
    pub(crate) fn set_waveform(&self, slot: usize, lut: Box<[Frame]>) -> Result<Option<Box<[Frame]>>> {
        if slot >= SLOTS || !wf::check(&lut) {
            return Err(Error::INVALID_ARG);
        }
        let lut: &'static [Frame] = Box::leak(lut);
        let waveform = Waveform::new(lut).inspect_err(|_| drop(unsafe { Box::from_raw(core::ptr::from_ref(lut).cast_mut()) }))?;
        self.drain();
        let mut guard = self.shared.lock.lock();
        while guard.active_px != 0 || guard.waiting_px != 0 {
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
        let pixels = image.pixels();
        let job = Job::Write { area, pixels: pixels.as_ptr(), len: pixels.len(), stride: image.stride_bytes(), format: self.format };
        self.progress.wait(self.submit(job));
        Ok(())
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
/// Writes queue for a task of their own and never wait for the glass: a
/// pixel still replaying takes what is written once a frame ends.
impl GramDriver for EpdLl {
    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        let stride = self.format.row_bytes(area.w as usize);
        Ok(self.submit(Job::Write { area, pixels: pixels.as_ptr(), len: pixels.len(), stride, format: self.format }))
    }

    fn wait(&self, ticket: Ticket) -> Result<()> {
        self.progress.wait(ticket);
        Ok(())
    }
}

/// In order with the writes queued before.
impl EpdDriver for EpdLl {
    fn set_mode(&self, mode: EpdMode) -> Result<()> {
        self.submit(Job::Mode(mode));
        Ok(())
    }

    fn refresh(&self, area: Rect, waveform: display::Waveform, all: bool) -> Result<()> {
        self.submit(Job::Refresh { area, waveform, all });
        Ok(())
    }

    fn clear(&self, area: Rect) -> Result<()> {
        self.submit(Job::Clear(area));
        Ok(())
    }

    fn refresh_unit(&self) -> RefreshUnit {
        RefreshUnit::Pixel
    }

    fn busy(&self, area: Rect) -> Result<bool> {
        Ok(self.shared.busy(area))
    }

    fn wait_idle(&self) -> Result<()> {
        self.drain();
        self.shared.wait_idle();
        Ok(())
    }
}
