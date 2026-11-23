/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! The i80 bus, the board's `Control` and the task replaying the waveforms.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::{CStr, c_void};
use core::slice;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU16, AtomicU32, Ordering};
use core::time::Duration;

use devkit_core::boxed;
use idf::heap::{self, Caps};
use idf::lcd::i80::{self, DmaBuffer, Io};
use idf::log::error;
use idf::sync::{Mutex, MutexGuard, Receiver, Semaphore, Sender, channel};
use idf::time::delay_us;
use idf::{Error, Result, gpio, sys, system, thread};

use super::waveform::{self as wf, ACT_RETIRE, ACTS_LEN, Acts, DC_ZERO, DcTab, Frame, G_FIRST, G_IDLE, G_PENDING, GROUPS, Group, Next, Pixel, REDIRECT, SLOTS, Waveform};
use crate::Absent;
use crate::display::{self, Driver, EpdDriver, EpdMode, GramDriver, Image, ImageFormat, Rect, RefreshUnit, Rotation, Size, Ticket};

const TAG: &CStr = c"epd_ll";

const SLOT_QUALITY: usize = 1;
const SLOT_CLEAR: usize = 3;
const QUEUE_DEPTH: usize = 4;
const DMA_BURST: usize = 64;
const TASK_STACK: usize = 6144;
const WRITE_STACK: usize = 4096;
/// Writes, refreshes and the like a writer may have queued.
const JOB_DEPTH: usize = 16;
const MAX_WAKES: u32 = 8;
const GATE_POLL: Duration = Duration::from_millis(1);

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
    /// Before a line goes out; from the bus interrupt when lines are queued
    /// back to back.
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
    /// A token per line queued to the bus, `QUEUE_DEPTH` in all; the bus
    /// interrupt gives each back once its line has gone out.
    free: Semaphore,
    /// Lines queued and not yet sent. Whoever takes it from 0 begins the next
    /// line; otherwise the interrupt of the line before begins it, before the
    /// bus starts it.
    queued: AtomicU32,
}

// The bus interrupt is not allocated IRAM-safe, so this may stay in flash.
unsafe extern "C" fn line_sent(_io: sys::esp_lcd_panel_io_handle_t, _data: *mut sys::esp_lcd_panel_io_event_data_t, ctx: *mut c_void) -> bool {
    let latch = unsafe { &*ctx.cast::<Latch>() };
    latch.control.line_latch();
    if latch.queued.fetch_sub(1, Ordering::AcqRel) > 1 {
        latch.control.line_begin();
    }
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

    /// Until every queued line has gone out. Blocks, rather than spinning, so
    /// the core stays with other tasks whatever this one's priority.
    fn wait(&self) {
        for _ in 0..QUEUE_DEPTH {
            self.latch.free.take();
        }
        for _ in 0..QUEUE_DEPTH {
            self.latch.free.give();
        }
    }

    fn frame_begin(&self) {
        self.wait();
        self.control().frame_begin();
    }

    /// Waits for a token, so the line queued `QUEUE_DEPTH` before has gone out
    /// and its buffer is free again.
    fn reserve(&self) {
        self.latch.free.take();
    }

    /// Queues `line` after a `reserve`; it stays in use until `QUEUE_DEPTH`
    /// more lines are reserved.
    fn write_line(&self, line: &DmaBuffer) {
        if self.latch.queued.fetch_add(1, Ordering::AcqRel) == 0 {
            self.control().line_begin();
        }
        let data = unsafe { slice::from_raw_parts(line.as_ptr(), self.tx_len) };
        if let Err(err) = unsafe { self.io.tx_color(-1, data) } {
            error!(TAG, "line: {err}");
            self.latch.queued.fetch_sub(1, Ordering::AcqRel);
            self.latch.free.give();
        }
    }

    fn frame_end(&self) {
        self.wait();
        self.control().frame_end();
    }
}

/// Written under `Shared::lock`. The counts wrap: a writer and the scan add
/// what they armed and take off what finished once a row or a frame is
/// through, in either order.
struct Counters {
    /// The replaying groups, from `G_FIRST`.
    groups: [Option<Group>; GROUPS],
    /// Pixels per replaying group.
    members: [u32; GROUPS],
    /// Per group, those who joined it and have yet to count their pixels.
    pins: [u8; GROUPS],
    active_px: u32,
    /// Mod 64.
    frame: u8,
    /// The task is driving rather than waiting on `wake`.
    running: bool,
    mode: EpdMode,
    /// Pixels with `REDIRECT`.
    waiting_px: u32,
    /// A refresh is arming an area for the next frame, which waits for it.
    gate: bool,
    /// The scan cuts SETTLE tails short this frame.
    skip_settle: bool,
    wf: [Option<Waveform>; SLOTS],
    /// The table of the slot was a `Box` given to `set_waveform`, leaked
    /// while in use.
    owned: [bool; SLOTS],
}

impl Counters {
    /// The group of pixels armed with `slot` to start at frame `start`,
    /// pinned, and whether it is new; none while every group replays.
    fn join(&mut self, slot: usize, start: u8) -> Option<(u8, bool)> {
        let group = Some(Group { slot, start });
        let replaying = usize::from(G_FIRST)..GROUPS;
        let (g, new) = match replaying.clone().find(|&g| self.groups[g] == group) {
            Some(g) => (g, false),
            None => (replaying.clone().find(|&g| self.groups[g].is_none())?, true),
        };
        self.groups[g] = group;
        self.pins[g] += 1;
        Some((g as u8, new))
    }

    fn release(&mut self, g: usize) {
        if self.members[g] == 0 && self.pins[g] == 0 {
            self.groups[g] = None;
        }
    }

    fn unpin(&mut self, g: u8) {
        let g = usize::from(g);
        self.pins[g] -= 1;
        self.release(g);
    }

    fn enter(&mut self, g: u8, n: u32) {
        let g = usize::from(g);
        self.members[g] = self.members[g].wrapping_add(n);
        self.active_px = self.active_px.wrapping_add(n);
        self.release(g);
    }

    fn leave(&mut self, g: u8, n: u32) {
        let g = usize::from(g);
        self.members[g] = self.members[g].wrapping_sub(n);
        self.active_px = self.active_px.wrapping_sub(n);
        self.release(g);
    }

    /// The waveform and start of a replaying `px`.
    fn replay_of(&self, px: u16) -> (Option<&Waveform>, u8) {
        match self.groups[usize::from(wf::group_of(px))] {
            Some(Group { slot, start }) => (self.wf[slot].as_ref(), start),
            None => (None, 0),
        }
    }
}

/// A row's pixels are read and written holding its flag. The scan takes each
/// in turn, so what a writer arms in a row not yet taken is driven by the
/// frame under way.
struct RowGuard<'a>(&'a AtomicBool);

impl Drop for RowGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct Shared {
    width: usize,
    height: usize,
    state: Box<[Pixel], Caps>,
    next: Box<[Next], Caps>,
    /// Internal RAM, as are the tables below: the scan reads them every row.
    row_flag: Box<[AtomicBool], Caps>,
    /// Pixels per row that are not idle.
    row_nonidle: Box<[AtomicU16], Caps>,
    /// Pixels per row with `REDIRECT`.
    row_redirect: Box<[AtomicU16], Caps>,
    /// `frame << 16 | rows`: the frame the scan is at and the rows it has
    /// taken; every row while it sleeps.
    pos: AtomicU32,
    /// Filled for the frame under way under `lock`: by the scan as the frame
    /// begins, and for a group that starts later by whoever joins it first.
    acts: Box<Acts, Caps>,
    retire: [AtomicPtr<DcTab>; GROUPS],
    cut: [AtomicPtr<DcTab>; GROUPS],
    has_wf: [AtomicBool; SLOTS],
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

fn idle_px(dc: u8, gray: u16) -> u16 {
    u16::from(G_IDLE) << 8 | u16::from(dc) << 4 | gray
}

fn pending_px(dc: u8, gray: u8) -> u16 {
    u16::from(G_PENDING) << 8 | u16::from(dc) << 4 | u16::from(gray)
}

/// Every group replays.
struct Busy;

/// What a writer changes in one row, under its flag.
struct RowDraw<'a> {
    shared: &'a Shared,
    y: usize,
    /// The slot written pixels are armed with, if the mode arms.
    slot: Option<usize>,
    all: bool,
    /// Joined when the first pixel needs it.
    group: Option<u8>,
    gained: u16,
    armed: u32,
    redirected: i32,
}

impl RowDraw<'_> {
    fn group(&mut self) -> core::result::Result<Option<u8>, Busy> {
        let Some(slot) = self.slot else { return Ok(None) };
        if self.group.is_none() {
            self.group = Some(self.shared.join_row(slot, self.y).ok_or(Busy)?);
        }
        Ok(self.group)
    }

    fn one(&mut self, px: &Pixel, next: &Next, gray: u8) -> core::result::Result<(), Busy> {
        let v = px.load(Ordering::Relaxed);
        let redirect = v & REDIRECT != 0;
        if wf::is_active(v) {
            if u16::from(gray) != v & 15 {
                next.store(wf::next_byte(gray, self.slot), Ordering::Relaxed);
                if !redirect {
                    px.store(v | REDIRECT, Ordering::Relaxed);
                    self.redirected += 1;
                }
            } else if redirect {
                px.store(v & !REDIRECT, Ordering::Relaxed);
                self.redirected -= 1;
            }
            return Ok(());
        }
        if redirect {
            // Pending, waiting for a group with the slot in `next`.
            match self.group()? {
                Some(g) => {
                    px.store(u16::from(g) << 8 | (v & 0x00F0) | u16::from(gray), Ordering::Relaxed);
                    self.armed += 1;
                    self.redirected -= 1;
                }
                None => {
                    px.store((v & !15) | u16::from(gray), Ordering::Relaxed);
                    next.store(next.load(Ordering::Relaxed) & 0xF0 | gray, Ordering::Relaxed);
                }
            }
            return Ok(());
        }
        let changed = v & 15 != u16::from(gray);
        if !(changed || self.slot.is_some() && (self.all || wf::group_of(v) == G_PENDING)) {
            return Ok(());
        }
        let d = wf::draw_px(v, gray, self.group()?, self.all);
        if d == v {
            return Ok(());
        }
        px.store(d, Ordering::Relaxed);
        self.gained += u16::from(wf::group_of(v) == G_IDLE);
        self.armed += u32::from(wf::is_active(d));
        Ok(())
    }

    /// Counts what changed; the row's flag is still held.
    fn finish(self) -> bool {
        let shared = self.shared;
        add(&shared.row_nonidle[self.y], self.gained);
        add(&shared.row_redirect[self.y], self.redirected as u16);
        if self.group.is_none() && self.redirected == 0 {
            return false;
        }
        let mut guard = shared.lock.lock();
        guard.waiting_px = guard.waiting_px.wrapping_add_signed(self.redirected);
        let mut wake = false;
        if let Some(g) = self.group {
            guard.enter(g, self.armed);
            guard.unpin(g);
            wake = self.armed != 0 && shared.start(&mut guard);
        }
        wake
    }
}

/// A group of the frame under way, or of the next, for a slot.
#[derive(Clone, Copy)]
enum Joined {
    Untried,
    /// No waveform in the slot.
    Absent,
    Busy,
    Group(u8),
}

/// What the scan changes in a frame, counted once it is through.
struct Pass<'a> {
    shared: &'a Shared,
    frame: u8,
    now: [Joined; SLOTS],
    later: [Joined; SLOTS],
    enter: [u32; GROUPS],
    leave: [u32; GROUPS],
    settled: u32,
    retired: u32,
    row_freed: u16,
    row_settled: u16,
}

impl<'a> Pass<'a> {
    fn new(shared: &'a Shared, frame: u8) -> Self {
        Self {
            shared,
            frame,
            now: [Joined::Untried; SLOTS],
            later: [Joined::Untried; SLOTS],
            enter: [0; GROUPS],
            leave: [0; GROUPS],
            settled: 0,
            retired: 0,
            row_freed: 0,
            row_settled: 0,
        }
    }

    /// The group pixels redirected to `slot` join: one at its first step this
    /// frame with `now`, else one that starts with the next.
    fn group(&mut self, slot: usize, now: bool) -> Joined {
        let cached = if now { self.now[slot] } else { self.later[slot] };
        if !matches!(cached, Joined::Untried) {
            return cached;
        }
        let shared = self.shared;
        let mut guard = shared.lock.lock();
        let start = if now { self.frame } else { wf::start_after(self.frame) };
        let joined = if guard.wf[slot].is_none() {
            Joined::Absent
        } else {
            match guard.join(slot, start) {
                Some((g, new)) => {
                    if new {
                        shared.fill_group(&guard, usize::from(g));
                    }
                    Joined::Group(g)
                }
                None => Joined::Busy,
            }
        };
        drop(guard);
        *(if now { &mut self.now[slot] } else { &mut self.later[slot] }) = joined;
        joined
    }

    fn act(&self, px: u16) -> u8 {
        self.shared.acts[usize::from(px & 0x0FFF)].load(Ordering::Relaxed)
    }

    fn dc(&self, tab: &AtomicPtr<DcTab>, px: u16) -> Option<u8> {
        let tab = tab.load(Ordering::Relaxed);
        // Null, or a table of a waveform that stays while its group replays.
        (!tab.is_null()).then(|| unsafe { &*tab }[usize::from(px & 0xFF)])
    }

    /// The action of pixel `i` this frame, as it finishes or goes on to what
    /// was written for it.
    fn pixel(&mut self, i: usize) -> u8 {
        let shared = self.shared;
        let px = &shared.state[i];
        let v = px.load(Ordering::Relaxed);
        let a = self.act(v);
        let g = wf::group_of(v);
        let redirect = v & REDIRECT != 0;
        if g < G_FIRST {
            if !redirect {
                return 0;
            }
            // Pending since no group was free.
            let n = shared.next[i].load(Ordering::Relaxed);
            return match wf::next_slot(n).map(|slot| self.group(slot, true)) {
                Some(Joined::Group(to)) => {
                    let armed = wf::in_group(v, to);
                    px.store(armed, Ordering::Relaxed);
                    self.enter[usize::from(to)] += 1;
                    self.settle();
                    self.act(armed) & 3
                }
                Some(Joined::Busy) => 0,
                _ => {
                    px.store(v & !REDIRECT, Ordering::Relaxed);
                    self.settle();
                    0
                }
            };
        }
        let retiring = a & ACT_RETIRE != 0;
        if !redirect {
            if retiring && let Some(dc) = self.dc(&shared.retire[usize::from(g)], v) {
                px.store(idle_px(dc, v & 15), Ordering::Relaxed);
                self.leave[usize::from(g)] += 1;
                self.retired += 1;
                self.row_freed += 1;
            }
            return a & 3;
        }
        let n = shared.next[i].load(Ordering::Relaxed);
        let (gray, slot) = (wf::next_gray(n), wf::next_slot(n));
        if retiring && let Some(dc) = self.dc(&shared.retire[usize::from(g)], v) {
            let pending = pending_px(dc, gray);
            self.leave[usize::from(g)] += 1;
            self.retired += 1;
            match slot.map(|slot| self.group(slot, false)) {
                Some(Joined::Group(to)) => {
                    px.store(wf::in_group(pending, to), Ordering::Relaxed);
                    self.enter[usize::from(to)] += 1;
                    self.settle();
                }
                Some(Joined::Busy) => px.store(pending | REDIRECT, Ordering::Relaxed),
                _ => {
                    px.store(pending, Ordering::Relaxed);
                    self.settle();
                }
            }
            return a & 3;
        }
        // Stopped before this frame's step, unless a STOP frame is ahead.
        let Some(dc) = self.dc(&shared.cut[usize::from(g)], v) else { return a & 3 };
        let pending = pending_px(dc, gray);
        match slot.map(|slot| self.group(slot, true)) {
            Some(Joined::Group(to)) => {
                let armed = wf::in_group(pending, to);
                px.store(armed, Ordering::Relaxed);
                self.leave[usize::from(g)] += 1;
                self.enter[usize::from(to)] += 1;
                self.settle();
                self.act(armed) & 3
            }
            // Goes on, and is stopped a frame later.
            Some(Joined::Busy) => a & 3,
            _ => {
                px.store(pending, Ordering::Relaxed);
                self.leave[usize::from(g)] += 1;
                self.settle();
                0
            }
        }
    }

    fn settle(&mut self) {
        self.settled += 1;
        self.row_settled += 1;
    }

    fn chunk(&mut self, first: usize, n: usize) -> [u8; 2] {
        let mut out = [0; 2];
        for k in 0..n {
            out[k / 4] |= self.pixel(first + k) << (6 - 2 * (k % 4));
        }
        out
    }

    /// Counts what changed in row `y`, its flag still held.
    fn row_done(&mut self, y: usize) {
        if self.row_freed != 0 {
            sub(&self.shared.row_nonidle[y], core::mem::take(&mut self.row_freed));
        }
        if self.row_settled != 0 {
            sub(&self.shared.row_redirect[y], core::mem::take(&mut self.row_settled));
        }
    }

    /// Counts the frame, and returns how many pixels finished.
    fn finish(self, counters: &mut Counters) -> u32 {
        for g in usize::from(G_FIRST)..GROUPS {
            if self.enter[g] != 0 {
                counters.enter(g as u8, self.enter[g]);
            }
        }
        for joined in self.now.iter().chain(&self.later) {
            if let Joined::Group(g) = joined {
                counters.unpin(*g);
            }
        }
        for g in usize::from(G_FIRST)..GROUPS {
            if self.leave[g] != 0 {
                counters.leave(g as u8, self.leave[g]);
            }
        }
        counters.waiting_px = counters.waiting_px.wrapping_sub(self.settled);
        self.retired
    }
}

impl Shared {
    fn row(&self, y: usize) -> &[Pixel] {
        &self.state[y * self.width..][..self.width]
    }

    fn lock_row(&self, y: usize) -> RowGuard<'_> {
        let flag = &self.row_flag[y];
        let mut spins = 0u32;
        while flag.swap(true, Ordering::Acquire) {
            spins += 1;
            // The holder may share this core.
            if spins % 2048 == 0 {
                thread::yield_now();
            } else {
                core::hint::spin_loop();
            }
        }
        RowGuard(flag)
    }

    /// The frame the scan is at and the rows it has taken.
    fn pos(&self) -> (u8, usize) {
        let pos = self.pos.load(Ordering::SeqCst);
        ((pos >> 16) as u8, (pos & 0xFFFF) as usize)
    }

    /// What group `g` does in the frame under way.
    fn fill_group(&self, counters: &Counters, g: usize) {
        let tab = wf::group_tab(&counters.wf, counters.groups[g], counters.frame, counters.skip_settle);
        wf::fill_acts(tab.rowset, !tab.retire.is_null(), &self.acts[g * 256..][..256]);
        self.retire[g].store(tab.retire.cast_mut(), Ordering::Release);
        self.cut[g].store(tab.cut.cast_mut(), Ordering::Release);
    }

    /// The group pixels of row `y`, whose flag is held, are armed in with
    /// `slot`: at its first step in the frame under way if the scan has yet
    /// to take the row, else starting with the next frame. Pinned.
    fn join_row(&self, slot: usize, y: usize) -> Option<u8> {
        let mut guard = self.lock.lock();
        let (frame, taken) = self.pos();
        let start = if taken <= y { frame } else { wf::start_after(frame) };
        let (g, new) = guard.join(slot, start)?;
        if new {
            self.fill_group(&guard, usize::from(g));
        }
        Some(g)
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

    fn wait_inactive(&self, x0: usize, y0: usize, x1: usize, y1: usize) {
        while self.area_has_active(x0, y0, x1, y1) {
            drop(self.wait_retire(self.lock.lock()));
        }
    }

    fn start(&self, counters: &mut Counters) -> bool {
        let wake = !counters.running;
        counters.running = true;
        wake
    }

    /// `pixels` are rows of `format`, L4 or L8, `stride` bytes apart.
    fn draw(&self, area: Rect, pixels: &[u8], stride: usize, format: ImageFormat) -> Result<()> {
        let (x0, y0, w, h) = (area.x as usize, area.y as usize, area.w as usize, area.h as usize);
        let row_bytes = format.row_bytes(w);
        let mode = self.lock.lock().mode;
        if mode == EpdMode::Seed {
            self.seed(area, pixels, stride, format);
            return Ok(());
        }
        let (slot, all) = match mode {
            EpdMode::Refresh(waveform) => (Some(slot_of(waveform)), false),
            EpdMode::RefreshAll(waveform) => (Some(slot_of(waveform)), true),
            _ => (None, false),
        };
        let slot = slot.filter(|&s| self.has_wf[s].load(Ordering::Relaxed));
        let mut wake = false;
        for r in 0..h {
            let y = y0 + r;
            let start = y * self.width + x0;
            let (row, next) = (&self.state[start..][..w], &self.next[start..][..w]);
            let src = &pixels[r * stride..][..row_bytes];
            loop {
                let flag = self.lock_row(y);
                let mut draw = RowDraw { shared: self, y, slot, all, group: None, gained: 0, armed: 0, redirected: 0 };
                let mut one = |((px, n), g): ((&Pixel, &Next), u8)| draw.one(px, n, g);
                let done = match format {
                    ImageFormat::L4 => row.iter().zip(next).zip(wf::grays_l4(src)).try_for_each(&mut one),
                    _ => row.iter().zip(next).zip(wf::grays_l8(src)).try_for_each(&mut one),
                };
                wake |= draw.finish();
                drop(flag);
                match done {
                    Ok(()) => break,
                    // What the row took so far stays; the rest once a group
                    // has finished.
                    Err(Busy) => drop(self.wait_retire(self.lock.lock())),
                }
            }
        }
        if wake {
            self.wake.give();
        }
        Ok(())
    }

    fn seed(&self, area: Rect, pixels: &[u8], stride: usize, format: ImageFormat) {
        let (x0, y0, w, h) = (area.x as usize, area.y as usize, area.w as usize, area.h as usize);
        let row_bytes = format.row_bytes(w);
        self.wait_inactive(x0, y0, x0 + w, y0 + h);
        let end: *const DcTab = self.lock.lock().wf[SLOT_QUALITY].as_ref().map_or(&wf::KEEP, |q| q.dc_after(q.steps()));
        // A waveform's tables stay while nothing replaces it, which waits
        // for the write task.
        let end = unsafe { &*end };
        let mut settled = 0u32;
        for r in 0..h {
            let y = y0 + r;
            let src = &pixels[r * stride..][..row_bytes];
            let _flag = self.lock_row(y);
            let (mut subsumed, mut row_settled) = (0u16, 0u16);
            let mut one = |x: usize, g: u8| {
                let px = &self.state[y * self.width + x];
                let v = px.load(Ordering::Relaxed);
                if wf::is_active(v) {
                    return;
                }
                subsumed += u16::from(wf::group_of(v) == G_PENDING);
                row_settled += u16::from(v & REDIRECT != 0);
                px.store(wf::seed_px(v, g, end), Ordering::Relaxed);
            };
            match format {
                ImageFormat::L4 => (x0..x0 + w).zip(wf::grays_l4(src)).for_each(|(x, g)| one(x, g)),
                _ => (x0..x0 + w).zip(wf::grays_l8(src)).for_each(|(x, g)| one(x, g)),
            }
            sub(&self.row_nonidle[y], subsumed);
            sub(&self.row_redirect[y], row_settled);
            settled += u32::from(row_settled);
        }
        if settled != 0 {
            let mut guard = self.lock.lock();
            guard.waiting_px = guard.waiting_px.wrapping_sub(settled);
        }
    }

    /// Joins the group that starts with the next frame, which then waits
    /// until `ungate`: the whole of an area starts together.
    fn gate(&self, slot: usize) -> u8 {
        let mut guard = self.lock.lock();
        loop {
            let start = wf::start_after(guard.frame);
            if let Some((g, new)) = guard.join(slot, start) {
                if new {
                    self.fill_group(&guard, usize::from(g));
                }
                guard.gate = true;
                return g;
            }
            guard = self.wait_retire(guard);
        }
    }

    fn ungate(&self, g: u8, armed: u32, left: &[u32; GROUPS], settled: u32) {
        let mut guard = self.lock.lock();
        guard.enter(g, armed);
        guard.unpin(g);
        for (from, &n) in left.iter().enumerate() {
            if n != 0 {
                guard.leave(from as u8, n);
            }
        }
        guard.waiting_px = guard.waiting_px.wrapping_sub(settled);
        guard.gate = false;
        let wake = armed != 0 && self.start(&mut guard);
        drop(guard);
        if wake {
            self.wake.give();
        }
    }

    /// Arms the pending pixels of `area`, and the idle ones too with `all`;
    /// those replaying go on, to what was written for them with `waveform`.
    fn refresh(&self, area: Rect, waveform: display::Waveform, all: bool) {
        let slot = slot_of(waveform);
        if !self.has_wf[slot].load(Ordering::Relaxed) {
            return;
        }
        let (x0, y0) = (area.x as usize, area.y as usize);
        let (x1, y1) = (x0 + area.w as usize, y0 + area.h as usize);
        let g = self.gate(slot);
        let (mut armed, mut settled) = (0u32, 0u32);
        for y in y0..y1 {
            if !all && self.row_nonidle[y].load(Ordering::Relaxed) == 0 {
                continue;
            }
            let start = y * self.width + x0;
            let (row, next) = (&self.state[start..][..x1 - x0], &self.next[start..][..x1 - x0]);
            let _flag = self.lock_row(y);
            let (mut row_armed, mut gained, mut row_settled) = (0u32, 0u16, 0u16);
            let mut one = |px: &Pixel, n: &Next| {
                let v = px.load(Ordering::Relaxed);
                match wf::group_of(v) {
                    G_PENDING => row_settled += u16::from(v & REDIRECT != 0),
                    G_IDLE if all => gained += 1,
                    G_IDLE => return,
                    _ => {
                        if v & REDIRECT != 0 {
                            n.store(wf::next_byte(wf::next_gray(n.load(Ordering::Relaxed)), Some(slot)), Ordering::Relaxed);
                        }
                        return;
                    }
                }
                px.store(wf::in_group(v, g), Ordering::Relaxed);
                row_armed += 1;
            };
            if all && row.as_ptr() as usize % 4 == 0 {
                let pairs = unsafe { slice::from_raw_parts(row.as_ptr().cast::<AtomicU32>(), row.len() / 2) };
                let both = u32::from(g) << 8 | u32::from(g) << 24;
                let mut idle_pairs = 0u32;
                for (p, (px, n)) in pairs.iter().zip(row.chunks_exact(2).zip(next.chunks_exact(2))) {
                    let w = p.load(Ordering::Relaxed);
                    if w & 0x1F00_1F00 == 0 {
                        p.store(w & 0x00FF_00FF | both, Ordering::Relaxed);
                        idle_pairs += 1;
                    } else {
                        one(&px[0], &n[0]);
                        one(&px[1], &n[1]);
                    }
                }
                if row.len() % 2 != 0 {
                    one(&row[row.len() - 1], &next[row.len() - 1]);
                }
                row_armed += idle_pairs * 2;
                gained += (idle_pairs * 2) as u16;
            } else {
                row.iter().zip(next).for_each(|(px, n)| one(px, n));
            }
            add(&self.row_nonidle[y], gained);
            sub(&self.row_redirect[y], row_settled);
            armed += row_armed;
            settled += u32::from(row_settled);
        }
        self.ungate(g, armed, &[0; GROUPS], settled);
    }

    fn clear(&self, area: Rect) {
        let (x0, y0) = (area.x as usize, area.y as usize);
        let (x1, y1) = (x0 + area.w as usize, y0 + area.h as usize);
        self.wait_inactive(x0, y0, x1, y1);
        let g = self.gate(SLOT_CLEAR);
        let mut left = [0u32; GROUPS];
        let mut settled = 0u32;
        for y in y0..y1 {
            let _flag = self.lock_row(y);
            let (mut gained, mut row_settled) = (0u16, 0u16);
            for px in &self.row(y)[x0..x1] {
                let v = px.load(Ordering::Relaxed);
                let from = wf::group_of(v);
                gained += u16::from(from == G_IDLE);
                row_settled += u16::from(v & REDIRECT != 0);
                if from >= G_FIRST {
                    left[usize::from(from)] += 1;
                }
                px.store(u16::from(g) << 8 | (v & 0x00F0) | 15, Ordering::Relaxed);
            }
            add(&self.row_nonidle[y], gained);
            sub(&self.row_redirect[y], row_settled);
            settled += u32::from(row_settled);
        }
        self.ungate(g, ((x1 - x0) * (y1 - y0)) as u32, &left, settled);
    }

    /// Whether `area` holds a pixel a draw could not stop.
    fn busy(&self, area: Rect) -> bool {
        let (x0, y0) = (area.x as usize, area.y as usize);
        let (x1, y1) = (x0 + area.w as usize, y0 + area.h as usize);
        let guard = self.lock.lock();
        let stopping = guard.groups.iter().flatten().any(|group| guard.wf[group.slot].as_ref().is_some_and(Waveform::stops));
        if !stopping {
            return false;
        }
        (y0..y1).any(|y| {
            self.row_nonidle[y].load(Ordering::Relaxed) != 0
                && self.row(y)[x0..x1].iter().any(|px| {
                    let v = px.load(Ordering::Relaxed);
                    let (replay, start) = guard.replay_of(v);
                    wf::is_active(v) && wf::cut_dc(replay, guard.frame, start).is_none()
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
    lines: [DmaBuffer; QUEUE_DEPTH],
    hold_line: DmaBuffer,
    line_bytes: usize,
}

fn run_frame(shared: &Shared, scan: &mut Scan) {
    let h = shared.height;
    let mut guard = shared.lock.lock();
    while guard.gate {
        drop(guard);
        thread::sleep(GATE_POLL);
        guard = shared.lock.lock();
    }
    guard.frame = (guard.frame + 1) & 63;
    guard.skip_settle = shared.waiters.load(Ordering::SeqCst) > 0;
    for g in 0..GROUPS {
        shared.fill_group(&guard, g);
    }
    let frame = guard.frame;
    shared.pos.store(u32::from(frame) << 16, Ordering::SeqCst);
    drop(guard);

    let bus = &scan.bus;
    let acts: &[AtomicU8; ACTS_LEN] = (&*shared.acts).try_into().expect("acts");
    let mut pass = Pass::new(shared, frame);
    bus.frame_begin();
    for y in 0..h {
        bus.reserve();
        let flag = shared.lock_row(y);
        shared.pos.store(u32::from(frame) << 16 | (y + 1) as u32, Ordering::SeqCst);
        let line = if shared.row_nonidle[y].load(Ordering::Relaxed) == 0 {
            drop(flag);
            &scan.hold_line
        } else {
            let line = &scan.lines[y % QUEUE_DEPTH];
            let dst = unsafe { slice::from_raw_parts_mut(line.as_ptr(), scan.line_bytes) };
            let first = y * shared.width;
            wf::blit_line(shared.row(y), acts, dst, |x, n| pass.chunk(first + x, n));
            pass.row_done(y);
            drop(flag);
            line
        };
        bus.write_line(line);
    }
    bus.frame_end();
    shared.pos.store(u32::from(frame) << 16 | h as u32, Ordering::SeqCst);

    let mut guard = shared.lock.lock();
    let retired = pass.finish(&mut guard);
    for g in usize::from(G_FIRST)..GROUPS {
        // Every pixel of a group that finished was in a row the scan took.
        if !shared.retire[g].load(Ordering::Relaxed).is_null() && guard.members[g] != 0 && guard.pins[g] == 0 {
            error!(TAG, "group {g}: {} pixels left", guard.members[g]);
            let members = guard.members[g];
            guard.leave(g as u8, members);
        }
    }
    drop(guard);
    if retired != 0 || shared.waiters.load(Ordering::SeqCst) > 0 {
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

fn cells<T>(len: usize, caps: Caps, new: impl Fn() -> T) -> Result<Box<[T], Caps>> {
    let mut cells = heap::try_vec(len, caps)?;
    cells.extend((0..len).map(|_| new()));
    Ok(cells.into_boxed_slice())
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
        let has_wf = core::array::from_fn(|slot| AtomicBool::new(slots[slot].is_some()));

        let state = cells(width * height, Caps::SPIRAM, || AtomicU16::new(idle_px(DC_ZERO as u8, 15)))?;
        let next = cells(width * height, Caps::SPIRAM, || Next::new(15))?;
        let row_flag = cells(height, Caps::INTERNAL, || AtomicBool::new(false))?;
        let row_nonidle = cells(height, Caps::INTERNAL, || AtomicU16::new(0))?;
        let row_redirect = cells(height, Caps::INTERNAL, || AtomicU16::new(0))?;
        let acts = cells(ACTS_LEN, Caps::INTERNAL, || AtomicU8::new(0))?;

        let wiring = Wiring {
            data: config.data,
            sph: config.sph,
            cl: config.cl,
            dc_dummy: config.dc_dummy,
            pclk_hz: config.pclk_hz,
            tx_len: usize::from(line_bytes) + usize::from(config.line_padding),
        };
        let depth = QUEUE_DEPTH as u32;
        let latch = Box::new(Latch { control: config.control, free: Semaphore::try_new(depth, depth)?, queued: AtomicU32::new(0) });
        let bus = Bus::new_off(&wiring, latch, config.core).inspect_err(|err| error!(TAG, "bus: {err}"))?;
        bus.control().power_off();
        let tx_len = bus.tx_len;
        let line = || bus.io.alloc_buffer(tx_len, Caps::DMA | Caps::INTERNAL);
        let (lines, hold_line) = ([line()?, line()?, line()?, line()?], line()?);
        // Straight into the heap: on the stack it would take more than a
        // default main task has.
        let scan = boxed!(Scan { bus, lines, hold_line, line_bytes: usize::from(line_bytes) });

        let shared = Arc::new(Shared {
            width,
            height,
            state,
            next,
            row_flag,
            row_nonidle,
            row_redirect,
            pos: AtomicU32::new(height as u32),
            acts,
            retire: [const { AtomicPtr::new(core::ptr::null_mut()) }; GROUPS],
            cut: [const { AtomicPtr::new(core::ptr::null_mut()) }; GROUPS],
            has_wf,
            lock: Mutex::new(Counters {
                groups: [None; GROUPS],
                members: [0; GROUPS],
                pins: [0; GROUPS],
                active_px: 0,
                frame: 0,
                running: false,
                mode: EpdMode::Hold,
                waiting_px: 0,
                gate: false,
                skip_settle: false,
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
        self.shared.has_wf[slot].store(true, Ordering::Relaxed);
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
