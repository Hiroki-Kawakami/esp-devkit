/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The IDF sdmmc driver brings the card up and waits for DAT1; every command
 * after that is issued here directly. Short ones are polled, long transfers
 * wait for the completion the IDF ISR queues.
 */

use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::ffi::{CStr, c_void};
use core::mem::{self, offset_of};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::time::Duration;

use idf::heap::{self, Caps};
use idf::log::{debug, error};
use idf::sdmmc::{self, SlotPins};
use idf::time::Instant;
use idf::{Error, Result, gpio, sys, thread};

use super::ffi;
use crate::driver::sdmmc::{Controller, PolledSlot};

const TAG: &CStr = c"esp_hosted_sdio";

pub(super) const BLOCK: usize = 512;

const REG_TOKEN_RDATA: u32 = 0x044;
const REG_INT_RAW: u32 = 0x050;
const REG_PKT_LEN: u32 = 0x060;
const REG_H2S_INT: u32 = 0x08C;
const REG_INT_CLR: u32 = 0x0D4;
const DATA_END_ADDR: u32 = 0x1F800;

const CCCR_FN_ENABLE: u32 = 0x02;
const CCCR_FN_READY: u32 = 0x03;
const CCCR_INT_ENABLE: u32 = 0x04;
const fn fbr_blksize(func: u32) -> u32 {
    0x100 * func + 0x10
}

const FUNC: u32 = 1;
const HOST_FLAG_ALLOC_ALIGNED_BUF: u32 = 1 << 6;

const SD_IO_RW_DIRECT: u32 = 52;
const SD_IO_RW_EXTENDED: u32 = 53;
const CMD52_WRITE: u32 = 1 << 31;
const CMD53_WRITE: u32 = 1 << 31;
const CMD53_BLOCK_MODE: u32 = 1 << 27;
const CMD53_INCREMENT: u32 = 1 << 26;
const ARG_FUNC_SHIFT: u32 = 28;
const ARG_REG_SHIFT: u32 = 9;
const ARG_REG_MASK: u32 = 0x1ffff;
const CMD53_LENGTH_MASK: u32 = 0x1ff;

const EV_RESP_ERR: u32 = 1 << 1;
const EV_CMD_DONE: u32 = 1 << 2;
const EV_DATA_OVER: u32 = 1 << 3;
const EV_RCRC: u32 = 1 << 6;
const EV_DCRC: u32 = 1 << 7;
const EV_RTO: u32 = 1 << 8;
const EV_DTO: u32 = 1 << 9;
const EV_HTO: u32 = 1 << 10;
const EV_SBE: u32 = 1 << 13;
const EV_EBE: u32 = 1 << 15;
const EV_ERR: u32 = EV_RESP_ERR | EV_RCRC | EV_DCRC | EV_RTO | EV_DTO | EV_HTO | EV_SBE | EV_EBE;
const EV_DONE: u32 = EV_CMD_DONE | EV_DATA_OVER;
const EV_SD_MASK: u32 = 0xffff;
const EV_DEFAULT: u32 = 0x1 | EV_RESP_ERR | EV_CMD_DONE | EV_DATA_OVER | EV_RCRC | EV_DCRC | EV_RTO | EV_DTO
    | EV_HTO | (1 << 12) | EV_SBE | EV_EBE;
const IDMAC_INTS: u32 = (1 << 8) | (1 << 1) | (1 << 0);

const R5_ERRORS: u32 = 0xcb;
const DESC_NUM: usize = 10;
const DMA_MAX_BUF_LEN: usize = 4096;
const CACHE_LINE: usize = 64;
const SMALL_LEN: usize = CACHE_LINE;
const TIMEOUT: Duration = Duration::from_millis(20);
const IRQ_MIN_LEN: usize = 4096;

const CACHE_DIR_C2M: i32 = 1 << 0;
const CACHE_DIR_M2C: i32 = 1 << 1;

unsafe extern "C" {
    // Not in the public headers.
    fn sdmmc_get_slot_handle(slot_id: i32) -> ffi::sd_host_slot_handle_t;
}

pub(super) const fn padded(len: usize) -> usize {
    len.div_ceil(BLOCK) * BLOCK
}

fn hw() -> *mut ffi::sdmmc_dev_t {
    &raw mut ffi::SDMMC
}

unsafe impl devkit::Zeroable for ffi::sdmmc_desc_t {}

fn ticks(d: Duration) -> u32 {
    (d.as_millis() as u64 * sdkconfig::CONFIG_FREERTOS_HZ as u64 / 1000) as u32 + 1
}

#[derive(Clone, Copy)]
pub(crate) struct Config {
    pub slot: i32,
    pub pins: SlotPins,
    pub freq_khz: i32,
    /// The coprocessor's EN, held low to reset it.
    pub reset_pin: Option<i32>,
}

pub(super) struct Status {
    pub token: u32,
    pub int_raw: u32,
    pub pkt_len: u32,
}

struct State {
    card: sys::sdmmc_card_t,
    desc: Box<[ffi::sdmmc_desc_t], Caps>,
    small: Box<[u8], Caps>,
    ctlr: *mut ffi::sd_host_sdmmc_ctlr_t,
}

/// Accesses go through the controller lock, which `state` relies on.
pub(super) struct Sdio {
    controller: &'static Controller,
    config: Config,
    state: UnsafeCell<State>,
    polled: AtomicBool,
    card_ready: AtomicBool,
    clock_div: AtomicU32,
}

unsafe impl Sync for Sdio {}
unsafe impl Send for Sdio {}

impl Sdio {
    pub(super) fn new(controller: &'static Controller, config: Config) -> Result<Self> {
        let desc = heap::zeroed_slice::<ffi::sdmmc_desc_t>(DESC_NUM, Caps::INTERNAL | Caps::DMA | Caps::CACHE_ALIGNED)?;
        let small = heap::zeroed_slice::<u8>(SMALL_LEN, Caps::INTERNAL | Caps::DMA | Caps::CACHE_ALIGNED)?;
        Ok(Self {
            controller,
            config,
            state: UnsafeCell::new(State { card: unsafe { mem::zeroed() }, desc, small, ctlr: ptr::null_mut() }),
            polled: AtomicBool::new(false),
            card_ready: AtomicBool::new(false),
            clock_div: AtomicU32::new(0),
        })
    }

    /// The controller lock must be held.
    #[allow(clippy::mut_from_ref)]
    unsafe fn state(&self) -> &mut State {
        unsafe { &mut *self.state.get() }
    }

    pub(super) fn init(&'static self) -> Result<()> {
        self.controller.set_polled(self)?;
        let _guard = self.controller.acquire();
        let slot = sdmmc::slot_config(&self.config.pins, 4, true);
        Error::check(unsafe { sys::sdmmc_host_init_slot(self.config.slot, &slot) })
    }

    fn reset_slave(&self) {
        let Some(pin) = self.config.reset_pin else { return };
        let _ = gpio::set_direction(pin, gpio::Mode::Output);
        let _ = gpio::set_level(pin, false);
        thread::sleep(Duration::from_millis(10));
        let _ = gpio::set_level(pin, true);
    }

    fn fn_init(card: &mut sys::sdmmc_card_t) -> Result<()> {
        let read = |card: &mut sys::sdmmc_card_t, reg| {
            let mut v = 0u8;
            Error::check(unsafe { sys::sdmmc_io_read_byte(card, 0, reg, &mut v) }).map(|_| v)
        };
        let write = |card: &mut sys::sdmmc_card_t, reg, v| {
            Error::check(unsafe { sys::sdmmc_io_write_byte(card, 0, reg, v, ptr::null_mut()) })
        };
        let v = read(card, CCCR_FN_ENABLE)?;
        write(card, CCCR_FN_ENABLE, v | (1 << FUNC))?;
        for i in 0.. {
            if read(card, CCCR_FN_READY)? & (1 << FUNC) != 0 {
                break;
            }
            if i == 10 {
                return Err(Error::TIMEOUT);
            }
            thread::sleep(Duration::from_millis(10));
        }
        let v = read(card, CCCR_INT_ENABLE)?;
        write(card, CCCR_INT_ENABLE, v | 1 | (1 << FUNC))?;
        for func in 0..=FUNC {
            write(card, fbr_blksize(func), (BLOCK & 0xff) as u8)?;
            write(card, fbr_blksize(func) + 1, (BLOCK >> 8) as u8)?;
        }
        Error::check(unsafe { sys::sdmmc_io_enable_int(card) })
    }

    /// Pulses EN, then retries card init for `timeout`.
    pub(super) fn connect(&self, timeout: Duration) -> Result<()> {
        self.reset_slave();
        let mut host = sdmmc::default_host(self.config.slot);
        host.max_freq_khz = self.config.freq_khz;
        host.flags |= HOST_FLAG_ALLOC_ALIGNED_BUF;

        let deadline = Instant::now() + timeout;
        loop {
            let guard = self.controller.acquire();
            self.card_ready.store(false, Ordering::Release);
            let state = unsafe { self.state() };
            state.card = unsafe { mem::zeroed() };
            let mut result = Error::check(unsafe { sys::sdmmc_card_init(&host, &mut state.card) });
            if result.is_ok() {
                result = Self::fn_init(&mut state.card);
            }
            self.clock_div.store(unsafe { ffi::sdmmc_ll_get_clock_div(hw()) }, Ordering::Relaxed);
            state.ctlr = unsafe { controller_of(self.config.slot) };
            self.card_ready.store(result.is_ok(), Ordering::Release);
            drop(guard);
            match result {
                Ok(()) => return Ok(()),
                Err(err) => debug!(TAG, "card init: {err}"),
            }
            if Instant::now() >= deadline {
                return Err(Error::TIMEOUT);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// `None` waits for good.
    pub(super) fn wait_int(&self, timeout: Option<Duration>) -> Result<()> {
        let ticks = timeout.map_or(u32::MAX, ticks);
        let card = unsafe { &raw mut (*self.state.get()).card };
        Error::check(unsafe { sys::sdmmc_io_wait_int(card, ticks) })
    }

    /// One CMD53 over TOKEN_RDATA .. PKT_LEN.
    pub(super) fn read_status(&self) -> Result<Status> {
        let _guard = self.controller.lock();
        let state = unsafe { self.state() };
        let len = (REG_PKT_LEN - REG_TOKEN_RDATA + 4) as usize;
        let small = state.small.as_mut_ptr();
        let result = unsafe { self.cmd53(state, false, REG_TOKEN_RDATA, small, len) };
        let word = |reg: u32| {
            let at = (reg - REG_TOKEN_RDATA) as usize;
            u32::from_le_bytes(state.small[at..at + 4].try_into().unwrap())
        };
        let status = Status { token: word(REG_TOKEN_RDATA), int_raw: word(REG_INT_RAW), pkt_len: word(REG_PKT_LEN) };
        result.map(|_| status)
    }

    pub(super) fn clear_int(&self, bits: u32) -> Result<()> {
        let _guard = self.controller.lock();
        for (i, b) in bits.to_le_bytes().into_iter().enumerate() {
            if b != 0 {
                self.cmd52(true, REG_INT_CLR + i as u32, b)?;
            }
        }
        Ok(())
    }

    pub(super) fn read_token(&self) -> Result<u32> {
        let _guard = self.controller.lock();
        let state = unsafe { self.state() };
        let small = state.small.as_mut_ptr();
        unsafe { self.cmd53(state, false, REG_TOKEN_RDATA, small, 4) }?;
        Ok(u32::from_le_bytes(state.small[..4].try_into().unwrap()))
    }

    pub(super) fn notify(&self, bit: u8) -> Result<()> {
        let _guard = self.controller.lock();
        self.cmd52(true, REG_H2S_INT, 1 << bit).map(|_| ())
    }

    /// `buf` holds `padded(len)` bytes, cache-line aligned and DMA capable.
    pub(super) unsafe fn read(&self, buf: *mut u8, len: usize) -> Result<()> {
        let _guard = self.controller.lock();
        let state = unsafe { self.state() };
        unsafe { self.cmd53(state, false, DATA_END_ADDR - len as u32, buf, padded(len)) }
    }

    /// `buf` holds `padded(len)` bytes, cache-line aligned and DMA capable.
    pub(super) unsafe fn write(&self, buf: *const u8, len: usize) -> Result<()> {
        let _guard = self.controller.lock();
        let state = unsafe { self.state() };
        unsafe { self.cmd53(state, true, DATA_END_ADDR - len as u32, buf.cast_mut(), padded(len)) }
    }

    fn cmd52(&self, write: bool, addr: u32, data: u8) -> Result<u8> {
        let mut cmd: ffi::sdmmc_hw_cmd_t = unsafe { mem::zeroed() };
        unsafe { cmd.__bindgen_anon_1.set_cmd_index(SD_IO_RW_DIRECT) };
        let arg = if write { CMD52_WRITE } else { 0 }
            | (FUNC << ARG_FUNC_SHIFT)
            | ((addr & ARG_REG_MASK) << ARG_REG_SHIFT)
            | u32::from(data);
        unsafe { issue(cmd, self.config.slot, arg, EV_CMD_DONE, ptr::null_mut()) }?;
        Ok((unsafe { ptr::read_volatile(&raw const (*hw()).resp[0]) } & 0xff) as u8)
    }

    /// Block mode when `len` is a whole number of blocks.
    unsafe fn cmd53(&self, state: &mut State, write: bool, addr: u32, buf: *mut u8, len: usize) -> Result<()> {
        let n = len.div_ceil(DMA_MAX_BUF_LEN);
        if n > DESC_NUM {
            return Err(Error::INVALID_SIZE);
        }
        let block = len % BLOCK == 0;
        let sync_len = len.next_multiple_of(CACHE_LINE);
        unsafe { msync(buf.cast(), sync_len, CACHE_DIR_C2M) };

        let desc = state.desc.as_mut_ptr();
        for i in 0..n {
            let chunk = (len - i * DMA_MAX_BUF_LEN).min(DMA_MAX_BUF_LEN);
            let d = unsafe { &mut *desc.add(i) };
            *d = unsafe { mem::zeroed() };
            d.__bindgen_anon_1.set_first_descriptor(u32::from(i == 0));
            d.__bindgen_anon_1.set_last_descriptor(u32::from(i == n - 1));
            d.__bindgen_anon_1.set_second_address_chained(1);
            d.__bindgen_anon_1.set_owned_by_idmac(1);
            d.__bindgen_anon_2.set_buffer1_size(chunk as u32);
            d.buffer1_ptr = unsafe { buf.add(i * DMA_MAX_BUF_LEN) }.cast();
            d.__bindgen_anon_3.next_desc_ptr =
                if i == n - 1 { ptr::null_mut() } else { unsafe { desc.add(i + 1) }.cast() };
        }
        unsafe {
            msync(desc.cast(), n * mem::size_of::<ffi::sdmmc_desc_t>(), CACHE_DIR_C2M);
            ffi::sdmmc_ll_set_data_transfer_len(hw(), len as u32);
            ffi::sdmmc_ll_set_block_size(hw(), if block { BLOCK as u32 } else { len as u32 });
            ffi::sdmmc_ll_set_desc_addr(hw(), desc as u32);
            ffi::sdmmc_ll_enable_dma(hw(), true);
            ffi::sdmmc_ll_poll_demand(hw());
        }

        let mut cmd: ffi::sdmmc_hw_cmd_t = unsafe { mem::zeroed() };
        unsafe {
            cmd.__bindgen_anon_1.set_cmd_index(SD_IO_RW_EXTENDED);
            cmd.__bindgen_anon_1.set_data_expected(1);
            cmd.__bindgen_anon_1.set_rw(u32::from(write));
        }
        let count = if block { (len / BLOCK) as u32 } else { (len % BLOCK) as u32 };
        let arg = if write { CMD53_WRITE } else { 0 }
            | (FUNC << ARG_FUNC_SHIFT)
            | if block { CMD53_BLOCK_MODE } else { 0 }
            | CMD53_INCREMENT
            | ((addr & ARG_REG_MASK) << ARG_REG_SHIFT)
            | (count & CMD53_LENGTH_MASK);
        let ctlr = if len >= IRQ_MIN_LEN { state.ctlr } else { ptr::null_mut() };
        let result = unsafe { issue(cmd, self.config.slot, arg, EV_DONE, ctlr) };
        unsafe {
            ffi::sdmmc_ll_clear_idsts_interrupt(hw(), ffi::sdmmc_ll_get_idsts_interrupt_raw(hw()));
            if !write {
                msync(buf.cast(), sync_len, CACHE_DIR_M2C);
            }
        }
        result
    }

    // The IDF ISR would consume the completion events polled here, and would
    // report the FIFO request bits the polled transfers leave raised. Handing
    // back the full SD event mask instead of the IDF default enables RXDR/TXDR:
    // with DMA into a busy PSRAM they fire per FIFO threshold, overflow the IDF
    // event queue and drop the completion (sd_host_wait_for_event 0x107).
    fn set_polled(&self, polled: bool) {
        unsafe {
            if !polled {
                ffi::sdmmc_ll_clear_interrupt(hw(), EV_SD_MASK);
                ffi::sdmmc_ll_clear_idsts_interrupt(hw(), ffi::sdmmc_ll_get_idsts_interrupt_raw(hw()));
            }
            ffi::sdmmc_ll_enable_interrupt(hw(), EV_SD_MASK, false);
            if !polled {
                ffi::sdmmc_ll_enable_interrupt(hw(), EV_DEFAULT, true);
            }
            ptr::write_volatile(&raw mut (*hw()).idinten.val, if polled { 0 } else { IDMAC_INTS });
        }
        self.polled.store(polled, Ordering::Relaxed);
    }
}

impl PolledSlot for Sdio {
    fn suspend(&self) {
        if self.polled.load(Ordering::Relaxed) {
            self.set_polled(false);
        }
    }

    // The host clock divider is shared by both slots and the IDF driver
    // reprograms it per transaction for the slot it talks to; one IDF command
    // here puts ours back before polling resumes.
    fn resume(&self) {
        if !self.card_ready.load(Ordering::Acquire) {
            return;
        }
        if unsafe { ffi::sdmmc_ll_get_clock_div(hw()) } != self.clock_div.load(Ordering::Relaxed) {
            let state = unsafe { self.state() };
            let mut v = 0u8;
            unsafe { sys::sdmmc_io_read_byte(&mut state.card, 0, CCCR_FN_ENABLE, &mut v) };
        }
        self.set_polled(true);
    }
}

unsafe fn msync(addr: *mut c_void, len: usize, dir: i32) {
    unsafe { sys::esp_cache_msync(addr, len, dir) };
}

unsafe fn controller_of(slot: i32) -> *mut ffi::sd_host_sdmmc_ctlr_t {
    let handle = unsafe { sdmmc_get_slot_handle(slot) };
    if handle.is_null() {
        return ptr::null_mut();
    }
    let slot = unsafe { handle.byte_sub(offset_of!(ffi::sd_host_sdmmc_slot_t, drv)) }.cast::<ffi::sd_host_sdmmc_slot_t>();
    unsafe { (*slot).ctlr }
}

fn recover() {
    unsafe {
        ffi::sdmmc_ll_reset_fifo(hw());
        ffi::sdmmc_ll_reset_dma(hw());
        let t0 = Instant::now();
        while !(ffi::sdmmc_ll_is_fifo_reset_done(hw()) && ffi::sdmmc_ll_is_dma_reset_done(hw())) && t0.elapsed() < TIMEOUT {}
        ffi::sdmmc_ll_clear_interrupt(hw(), EV_ERR | EV_DONE);
        ffi::sdmmc_ll_clear_idsts_interrupt(hw(), ffi::sdmmc_ll_get_idsts_interrupt_raw(hw()));
    }
}

fn status_of(st: u32, done: u32) -> Option<Result<()>> {
    if st & EV_ERR != 0 {
        return Some(Err(if st & (EV_RCRC | EV_DCRC) != 0 { Error::INVALID_CRC } else { Error::TIMEOUT }));
    }
    (st & done == done).then_some(Ok(()))
}

fn wait_polled(done: u32, t0: Instant) -> Result<()> {
    loop {
        let st = unsafe { ffi::sdmmc_ll_get_interrupt_raw(hw()) };
        if let Some(result) = status_of(st, done) {
            if result.is_ok() {
                unsafe { ffi::sdmmc_ll_clear_interrupt(hw(), done) };
            }
            return result;
        }
        if t0.elapsed() > TIMEOUT {
            return Err(Error::TIMEOUT);
        }
    }
}

// The IDF ISR clears what it reports, so once enabled the events only come
// through its queue.
unsafe fn wait_irq(ctlr: *mut ffi::sd_host_sdmmc_ctlr_t, done: u32) -> Result<()> {
    let queue: sys::QueueHandle_t = unsafe { (*ctlr).event_queue }.cast();
    let mut st = 0;
    let result = loop {
        let mut ev: ffi::sd_host_sdmmc_event_t = unsafe { mem::zeroed() };
        if unsafe { sys::xQueueReceive(queue, (&raw mut ev).cast(), ticks(TIMEOUT)) } == 0 {
            break Err(Error::TIMEOUT);
        }
        st |= ev.sdmmc_status;
        if let Some(result) = status_of(st, done) {
            break result;
        }
    };
    unsafe { ffi::sdmmc_ll_enable_interrupt(hw(), EV_ERR | EV_DONE, false) };
    result
}

// The ISR drops events while the controller's current slot has no driver.
unsafe fn irq_usable(ctlr: *mut ffi::sd_host_sdmmc_ctlr_t) -> bool {
    !ctlr.is_null() && unsafe { !(*ctlr).slot[(*ctlr).cur_slot_id as usize].is_null() }
}

/// `ctlr` non-null waits for the IDF ISR's completion event instead of polling.
unsafe fn issue(mut cmd: ffi::sdmmc_hw_cmd_t, slot: i32, arg: u32, done: u32, ctlr: *mut ffi::sd_host_sdmmc_ctlr_t) -> Result<()> {
    let t0 = Instant::now();
    while !unsafe { ffi::sdmmc_ll_is_command_taken(hw()) } {
        if t0.elapsed() > TIMEOUT {
            return Err(Error::TIMEOUT);
        }
    }
    unsafe {
        let c = &mut cmd.__bindgen_anon_1;
        c.set_card_num(slot as u32);
        c.set_use_hold_reg(1);
        c.set_wait_complete(1);
        c.set_response_expect(1);
        c.set_check_response_crc(1);
        c.set_start_command(1);
    }
    let irq = unsafe { irq_usable(ctlr) };
    if irq {
        unsafe {
            sys::xQueueGenericReset((*ctlr).event_queue.cast(), 0);
            ffi::sdmmc_ll_enable_interrupt(hw(), EV_ERR | EV_DONE, true);
        }
    }
    unsafe {
        ffi::sdmmc_ll_set_command_arg(hw(), arg);
        // sdmmc_ll_set_command() memcpy()s through a non-volatile pointer,
        // which the compiler may store ahead of the argument: the command then
        // runs with the previous argument and the late argument write raises HLE.
        ptr::write_volatile(&raw mut (*hw()).cmd.val, cmd.val);
    }

    let mut result = if irq { unsafe { wait_irq(ctlr, done) } } else { wait_polled(done, t0) };
    if result.is_ok() && (unsafe { ptr::read_volatile(&raw const (*hw()).resp[0]) } >> 8) & R5_ERRORS != 0 {
        result = Err(Error::INVALID_RESPONSE);
    }
    if let Err(err) = result {
        error!(TAG, "CMD{} {arg:08x}: {err}", unsafe { cmd.__bindgen_anon_1.cmd_index() });
        recover();
    }
    result
}
