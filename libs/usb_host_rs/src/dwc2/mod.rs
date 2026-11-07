/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The DWC2 (Synopsys DesignWare OTG) controller in scatter/gather DMA mode,
 * through IDF's HAL.
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{CStr, c_void};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use idf::cache::{self, Flags};
use idf::executor::{self, Sender};
use idf::heap::Caps;
use idf::intr::{self, Interrupt};
use idf::log::{error, warn};
use idf::sync::{OnceLock, Spinlock};
use idf::thread;
use idf::time::Instant;

use crate::desc::TransferType;
use crate::hcd::{Hcd, PipeConfig, PipeId, PortEvent, Request};
use crate::{Error, Result, Speed};

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, unsafe_op_in_unsafe_fn)]
#[allow(clippy::all)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const TAG: &CStr = c"usb_host";

const PORT: i32 = 0;
const CTRL_QTDS: usize = 3;
const BULK_QTDS: usize = 2;
const QTD_XFER_SIZE: u32 = 0x1_ffff;
/// One bulk descriptor's worth, kept a multiple of every max packet size.
const MAX_BULK_BYTES: usize = 65536;
const SETUP_BYTES: usize = 8;
const CHAN_FLAG_ACTIVE: u32 = 1 << 0;
const CHAN_FLAG_HALT_REQUESTED: u32 = 1 << 1;

const INIT_DELAY: Duration = Duration::from_millis(30);
const DEBOUNCE: Duration = Duration::from_millis(250);
const RESET_HOLD: Duration = Duration::from_millis(30);
const RESET_RECOVERY: Duration = Duration::from_millis(30);
const HALT_TIMEOUT: Duration = Duration::from_millis(200);
const HALT_POLL: Duration = Duration::from_millis(1);

const DMA_INTERNAL: Caps = Caps::DMA.union(Caps::INTERNAL);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Active,
    /// A cancel halts the channel; the active request completes as
    /// `Canceled` once it has, and the next one starts.
    Canceling,
    /// `pipe_free` waits for the channel to halt.
    Halting,
    /// After an error on a bulk endpoint, until `clear`.
    Halted,
    Dead,
}

/// Requests linked through `Request::link`, only touched under the lock.
struct Queue {
    head: Option<Arc<Request>>,
    tail: *const Request,
}

impl Queue {
    const fn new() -> Self {
        Self { head: None, tail: ptr::null() }
    }

    fn push(&mut self, request: Arc<Request>) {
        let raw = Arc::as_ptr(&request);
        if self.head.is_none() {
            self.head = Some(request);
        } else {
            unsafe { *(*self.tail).link.get() = Some(request) };
        }
        self.tail = raw;
    }

    fn pop(&mut self) -> Option<Arc<Request>> {
        let head = self.head.take()?;
        self.head = unsafe { (*head.link.get()).take() };
        if self.head.is_none() {
            self.tail = ptr::null();
        }
        Some(head)
    }

    fn remove(&mut self, target: &Arc<Request>) -> Option<Arc<Request>> {
        let mut rest = Queue::new();
        let mut found = None;
        while let Some(r) = self.pop() {
            if found.is_none() && Arc::ptr_eq(&r, target) {
                found = Some(r);
            } else {
                rest.push(r);
            }
        }
        *self = rest;
        found
    }

    fn append(&mut self, other: &mut Queue) {
        while let Some(r) = other.pop() {
            self.push(r);
        }
    }
}

struct Pipe {
    chan: ffi::usb_dwc_hal_chan_t,
    ep_char: ffi::usb_dwc_hal_ep_char_t,
    has_chan: bool,
    control: bool,
    data_in: bool,
    endpoint: u8,
    mps: u16,
    qtds: *mut ffi::usb_dwc_ll_dma_qtd_t,
    qtds_nc: *mut ffi::usb_dwc_ll_dma_qtd_t,
    qtd_len: usize,
    setup: *mut u8,
    setup_nc: *mut u8,
    pending: Queue,
    active: Option<Arc<Request>>,
    state: State,
    ctrl_stage: i32,
    ctrl_in: bool,
    ctrl_skip: bool,
    ctrl_length: usize,
    halted: AtomicBool,
}

impl Pipe {
    fn chan_active(&self) -> bool {
        unsafe { self.chan.flags.val & CHAN_FLAG_ACTIVE != 0 }
    }

    fn clear_chan_flags(&mut self, flags: u32) {
        unsafe { self.chan.flags.val &= !flags };
    }

    fn remaining(&self, index: usize) -> usize {
        (unsafe { (*self.qtds_nc.add(index)).__bindgen_anon_1.buffer_status_val } & QTD_XFER_SIZE) as usize
    }

    fn clear_qtds(&mut self) {
        unsafe { ptr::write_bytes(self.qtds_nc, 0, self.qtd_len) };
    }
}

struct Inner {
    hal: ffi::usb_dwc_hal_context_t,
    frame_list: *mut u32,
    frame_bytes: usize,
    fifo: ffi::usb_dwc_hal_fifo_config_t,
    events: Option<Sender<PortEvent>>,
    speed: Speed,
    enabled: bool,
    resetting: bool,
    pipes: Vec<Option<Box<Pipe>>>,
    /// Finished under the lock, completed right after it.
    done: Queue,
    /// Completed, dropped from task context: the last reference may be
    /// ours, and an interrupt must not free.
    retired: Queue,
    port_events: [Option<PortEvent>; 4],
}

pub(crate) struct Dwc2 {
    inner: Spinlock<Option<Inner>>,
    intr: OnceLock<Interrupt>,
}

unsafe impl Send for Inner {}

fn finish(inner: &mut Inner, request: Arc<Request>, result: Result<usize>) {
    unsafe { *request.outcome.get() = Some(result) };
    inner.done.push(request);
}

fn cancel_all(inner: &mut Inner, index: usize, result: Error) {
    let Some(pipe) = inner.pipes[index].as_mut() else { return };
    let mut queue = core::mem::replace(&mut pipe.pending, Queue::new());
    let active = pipe.active.take();
    pipe.clear_qtds();
    if let Some(active) = active {
        finish(inner, active, Err(result));
    }
    while let Some(r) = queue.pop() {
        finish(inner, r, Err(result));
    }
}

fn chan_error(error: ffi::usb_dwc_hal_chan_error_t) -> Error {
    match error {
        ffi::usb_dwc_hal_chan_error_t_USB_DWC_HAL_CHAN_ERROR_STALL => Error::Stall,
        ffi::usb_dwc_hal_chan_error_t_USB_DWC_HAL_CHAN_ERROR_PKT_BBL => Error::Overflow,
        _ => Error::Failed,
    }
}

/// Fails only on memory that is not cached, which needs no sync.
unsafe fn msync(addr: *mut u8, len: usize, flags: Flags) {
    let _ = unsafe { cache::msync(addr, len, flags) };
}

impl Dwc2 {
    pub(crate) const fn new() -> Self {
        Self { inner: Spinlock::new(None), intr: OnceLock::new() }
    }

    fn set_interrupt(&self, on: bool) {
        if let Some(intr) = self.intr.get() {
            let _ = if on { intr.enable() } else { intr.disable() };
        }
    }

    /// Runs `f` under the lock, then completes what it finished and posts
    /// the port events it raised.
    fn locked<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let (out, mut done, events, sender) = self.inner.with(|inner| {
            let inner = inner.as_mut().unwrap();
            let out = f(inner);
            let done = core::mem::replace(&mut inner.done, Queue::new());
            (out, done, core::mem::take(&mut inner.port_events), inner.events.clone())
        });
        let mut completed = Queue::new();
        while let Some(r) = done.pop() {
            let result = unsafe { (*r.outcome.get()).take() }.unwrap_or(Err(Error::Failed));
            r.complete(result);
            completed.push(r);
        }
        if completed.head.is_some() {
            self.inner.with(|inner| inner.as_mut().unwrap().retired.append(&mut completed));
        }
        if let Some(sender) = sender {
            for event in events.into_iter().flatten() {
                if sender.try_send(event).is_err() {
                    error!(TAG, "port event {event:?} dropped");
                }
            }
        }
        out
    }

    fn initialized(&self) -> bool {
        self.inner.with(|inner| inner.is_some())
    }

    /// Drops what the interrupt retired; from task context only.
    fn reap(&self) {
        if !self.initialized() {
            return;
        }
        let mut retired = self.locked(|inner| core::mem::replace(&mut inner.retired, Queue::new()));
        while retired.pop().is_some() {}
    }

    fn power_on(inner: &mut Inner) {
        unsafe {
            ffi::usb_dwc_hal_port_init(&mut inner.hal);
            ffi::usb_dwc_hal_port_toggle_power(&mut inner.hal, true);
        }
    }

    fn sync_frame_list(inner: &Inner) {
        unsafe { msync(inner.frame_list.cast(), inner.frame_bytes, Flags::C2M) };
    }

    fn start(inner: &mut Inner, index: usize) {
        let enabled = inner.enabled;
        let Some(pipe) = inner.pipes[index].as_mut() else { return };
        if !pipe.has_chan || !enabled || pipe.state != State::Active || pipe.active.is_some() {
            return;
        }
        let Some(request) = pipe.pending.pop() else { return };
        if pipe.control {
            Self::start_control(pipe, &request);
        } else {
            Self::start_bulk(pipe, &request);
        }
        pipe.active = Some(request);
    }

    fn start_control(pipe: &mut Pipe, request: &Request) {
        let setup = request.setup.unwrap();
        unsafe { ptr::copy_nonoverlapping(setup.bytes().as_ptr(), pipe.setup_nc, SETUP_BYTES) };
        pipe.ctrl_in = setup.is_in();
        pipe.ctrl_skip = request.len == 0;
        pipe.ctrl_stage = 0;
        let (data, capacity) = unsafe { request.dma() };
        let mut length = request.len;
        if pipe.ctrl_in && pipe.mps > 0 {
            let rounded = length.next_multiple_of(usize::from(pipe.mps));
            if rounded <= capacity {
                length = rounded;
            }
        }
        pipe.ctrl_length = length;
        let q = pipe.qtds_nc;
        unsafe {
            ffi::usb_dwc_ll_qtd_set_out(q, pipe.setup, SETUP_BYTES as i32, true, true);
            if pipe.ctrl_skip {
                ffi::usb_dwc_ll_qtd_set_null(q.add(1));
            } else if pipe.ctrl_in {
                ffi::usb_dwc_ll_qtd_set_in(q.add(1), data, length as i32, true);
            } else {
                ffi::usb_dwc_ll_qtd_set_out(q.add(1), data, length as i32, true, false);
            }
            if pipe.ctrl_in && !pipe.ctrl_skip {
                ffi::usb_dwc_ll_qtd_set_out(q.add(2), ptr::null_mut(), 0, true, false);
            } else {
                ffi::usb_dwc_ll_qtd_set_in(q.add(2), ptr::null_mut(), 0, true);
            }
            ffi::usb_dwc_hal_chan_set_dir(&mut pipe.chan, false);
            ffi::usb_dwc_hal_chan_set_pid(&mut pipe.chan, 0);
            ffi::usb_dwc_hal_chan_activate(&mut pipe.chan, pipe.qtds.cast(), CTRL_QTDS as i32, 0);
        }
    }

    fn start_bulk(pipe: &mut Pipe, request: &Request) {
        let (data, capacity) = unsafe { request.dma() };
        let q = pipe.qtds_nc;
        let mut count = 1;
        unsafe {
            if pipe.data_in {
                let mut length = request.len;
                let rounded = length.next_multiple_of(usize::from(pipe.mps.max(1)));
                if rounded <= capacity && rounded <= MAX_BULK_BYTES {
                    length = rounded;
                }
                ffi::usb_dwc_ll_qtd_set_in(q, data, length as i32, true);
            } else {
                let zlp = request.zlp && request.len > 0 && request.len % usize::from(pipe.mps.max(1)) == 0;
                ffi::usb_dwc_ll_qtd_set_out(q, data, request.len as i32, !zlp, false);
                if zlp {
                    ffi::usb_dwc_ll_qtd_set_out(q.add(1), ptr::null_mut(), 0, true, false);
                    count = 2;
                }
            }
            ffi::usb_dwc_hal_chan_activate(&mut pipe.chan, pipe.qtds.cast(), count, 0);
        }
    }

    fn finish_active(inner: &mut Inner, index: usize, result: Result<usize>) {
        let Some(pipe) = inner.pipes[index].as_mut() else { return };
        let Some(request) = pipe.active.take() else { return };
        let result = result.map(|_| {
            let moved = if !pipe.control {
                Self::bulk_moved(pipe, &request)
            } else if pipe.ctrl_skip {
                0
            } else {
                pipe.ctrl_length.saturating_sub(pipe.remaining(1))
            };
            moved.min(request.len)
        });
        pipe.clear_qtds();
        finish(inner, request, result);
    }

    fn bulk_moved(pipe: &Pipe, request: &Request) -> usize {
        let (_, capacity) = unsafe { request.dma() };
        let asked = if pipe.data_in {
            let rounded = request.len.next_multiple_of(usize::from(pipe.mps.max(1)));
            if rounded <= capacity && rounded <= MAX_BULK_BYTES { rounded } else { request.len }
        } else {
            request.len
        };
        asked.saturating_sub(pipe.remaining(0))
    }

    fn handle_port(inner: &mut Inner, event: ffi::usb_dwc_hal_port_event_t) {
        let post = |inner: &mut Inner, e: PortEvent| {
            if let Some(slot) = inner.port_events.iter_mut().find(|s| s.is_none()) {
                *slot = Some(e);
            }
        };
        match event {
            ffi::usb_dwc_hal_port_event_t_USB_DWC_HAL_PORT_EVENT_CONN => post(inner, PortEvent::Connected),
            ffi::usb_dwc_hal_port_event_t_USB_DWC_HAL_PORT_EVENT_DISCONN => {
                inner.enabled = false;
                for index in 0..inner.pipes.len() {
                    cancel_all(inner, index, Error::NoDevice);
                    if let Some(pipe) = inner.pipes[index].as_mut() {
                        pipe.state = State::Dead;
                    }
                }
                post(inner, PortEvent::Disconnected);
            }
            ffi::usb_dwc_hal_port_event_t_USB_DWC_HAL_PORT_EVENT_ENABLED => {
                unsafe { ffi::usb_dwc_hal_port_enable(&mut inner.hal) };
                inner.speed = match unsafe { ffi::usb_dwc_hal_port_get_conn_speed(&mut inner.hal) } {
                    ffi::usb_dwc_speed_t_USB_DWC_SPEED_HIGH => Speed::High,
                    ffi::usb_dwc_speed_t_USB_DWC_SPEED_LOW => Speed::Low,
                    _ => Speed::Full,
                };
                inner.enabled = true;
            }
            ffi::usb_dwc_hal_port_event_t_USB_DWC_HAL_PORT_EVENT_DISABLED => {
                inner.enabled = false;
                if !inner.resetting {
                    post(inner, PortEvent::Disconnected);
                }
            }
            ffi::usb_dwc_hal_port_event_t_USB_DWC_HAL_PORT_EVENT_OVRCUR
            | ffi::usb_dwc_hal_port_event_t_USB_DWC_HAL_PORT_EVENT_OVRCUR_CLR => {
                unsafe { ffi::usb_dwc_hal_port_toggle_power(&mut inner.hal, false) };
                inner.enabled = false;
                post(inner, PortEvent::Disconnected);
            }
            _ => {}
        }
    }

    fn handle_channel(inner: &mut Inner, index: usize) {
        let Some(pipe) = inner.pipes[index].as_mut() else { return };
        if !pipe.has_chan {
            return;
        }
        let event = unsafe { ffi::usb_dwc_hal_chan_decode_intr(&mut pipe.chan) };
        match event {
            ffi::usb_dwc_hal_chan_event_t_USB_DWC_HAL_CHAN_EVENT_CPLT => {
                if pipe.active.is_none() {
                    return;
                }
                if pipe.control && pipe.ctrl_stage < 2 {
                    let data_in = if pipe.ctrl_stage == 0 && pipe.ctrl_skip {
                        pipe.ctrl_stage = 2;
                        true
                    } else if pipe.ctrl_stage == 0 {
                        pipe.ctrl_stage = 1;
                        pipe.ctrl_in
                    } else {
                        pipe.ctrl_stage = 2;
                        !pipe.ctrl_in
                    };
                    unsafe {
                        ffi::usb_dwc_hal_chan_set_dir(&mut pipe.chan, data_in);
                        ffi::usb_dwc_hal_chan_set_pid(&mut pipe.chan, 1);
                        ffi::usb_dwc_hal_chan_activate(
                            &mut pipe.chan,
                            pipe.qtds.cast(),
                            CTRL_QTDS as i32,
                            pipe.ctrl_stage,
                        );
                    }
                    return;
                }
                if pipe.state == State::Canceling {
                    pipe.state = State::Active;
                }
                Self::finish_active(inner, index, Ok(0));
                Self::start(inner, index);
            }
            ffi::usb_dwc_hal_chan_event_t_USB_DWC_HAL_CHAN_EVENT_ERROR => {
                let error = chan_error(unsafe { ffi::usb_dwc_hal_chan_get_error(&mut pipe.chan) });
                // The next SETUP clears a control endpoint's STALL or error.
                if pipe.control {
                    if pipe.state == State::Canceling {
                        pipe.state = State::Active;
                    }
                } else if pipe.state != State::Dead && pipe.state != State::Halting {
                    pipe.state = State::Halted;
                }
                Self::finish_active(inner, index, Err(error));
                Self::start(inner, index);
            }
            ffi::usb_dwc_hal_chan_event_t_USB_DWC_HAL_CHAN_EVENT_HALT_REQ => match pipe.state {
                State::Canceling => {
                    pipe.state = State::Active;
                    Self::finish_active(inner, index, Err(Error::Canceled));
                    Self::start(inner, index);
                }
                State::Halting => {
                    pipe.halted.store(true, Ordering::Release);
                }
                _ => {}
            },
            _ => {}
        }
    }

    unsafe extern "C" fn isr(arg: *mut c_void) {
        let this = unsafe { &*(arg as *const Dwc2) };
        this.locked(|inner| {
            let event = unsafe { ffi::usb_dwc_hal_decode_intr(&mut inner.hal) };
            if event == ffi::usb_dwc_hal_port_event_t_USB_DWC_HAL_PORT_EVENT_CHAN {
                loop {
                    let chan = unsafe { ffi::usb_dwc_hal_get_chan_pending_intr(&mut inner.hal) };
                    if chan.is_null() {
                        break;
                    }
                    let index = unsafe { ffi::usb_dwc_hal_chan_get_context(chan) } as usize;
                    Self::handle_channel(inner, index);
                }
            } else if event != ffi::usb_dwc_hal_port_event_t_USB_DWC_HAL_PORT_EVENT_NONE {
                Self::handle_port(inner, event);
            }
        });
    }

    fn alloc_dma(bytes: usize, align: usize) -> *mut u8 {
        unsafe { ffi::heap_caps_aligned_calloc(align, 1, bytes, DMA_INTERNAL.bits()).cast() }
    }

    fn free_pipe_memory(pipe: &Pipe) {
        unsafe {
            ffi::heap_caps_free(pipe.qtds.cast());
            if !pipe.setup.is_null() {
                ffi::heap_caps_free(pipe.setup.cast());
            }
        }
    }

    /// Halts `index`'s channel if it runs, waiting for it.
    fn halt_and_wait(&self, index: usize) {
        let wait = self.locked(|inner| {
            let Some(pipe) = inner.pipes[index].as_mut() else { return false };
            if pipe.state == State::Dead || !pipe.has_chan {
                return false;
            }
            pipe.state = State::Halting;
            pipe.halted.store(false, Ordering::Release);
            pipe.chan_active() && !unsafe { ffi::usb_dwc_hal_chan_request_halt(&mut pipe.chan) }
        });
        if !wait {
            return;
        }
        let deadline = Instant::now() + HALT_TIMEOUT;
        loop {
            let halted = self.locked(|inner| inner.pipes[index].as_ref().is_none_or(|p| p.halted.load(Ordering::Acquire)));
            if halted {
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(HALT_POLL);
        }
        self.locked(|inner| {
            if let Some(pipe) = inner.pipes[index].as_mut() {
                warn!(TAG, "endpoint {:02x} did not halt", pipe.endpoint);
                pipe.clear_chan_flags(CHAN_FLAG_ACTIVE | CHAN_FLAG_HALT_REQUESTED);
            }
        });
    }
}

impl Hcd for Dwc2 {
    fn init(&'static self, events: Sender<PortEvent>) -> Result<()> {
        let mut phy_config: ffi::usb_phy_config_t = unsafe { core::mem::zeroed() };
        phy_config.controller = ffi::usb_phy_controller_t_USB_PHY_CTRL_OTG;
        phy_config.target = if cfg!(any(CONFIG_IDF_TARGET_ESP32P4, CONFIG_IDF_TARGET_ESP32S31)) {
            ffi::usb_phy_target_t_USB_PHY_TARGET_UTMI
        } else {
            ffi::usb_phy_target_t_USB_PHY_TARGET_INT
        };
        phy_config.otg_mode = ffi::usb_otg_mode_t_USB_OTG_MODE_HOST;
        phy_config.otg_speed = ffi::usb_phy_speed_t_USB_PHY_SPEED_UNDEFINED;
        let mut phy: ffi::usb_phy_handle_t = ptr::null_mut();
        let err = unsafe { ffi::usb_new_phy(&phy_config, &mut phy) };
        if err != 0 {
            error!(TAG, "usb_new_phy: {err}");
            return Err(Error::Failed);
        }

        let mut hal: ffi::usb_dwc_hal_context_t = unsafe { core::mem::zeroed() };
        unsafe { ffi::usb_dwc_hal_init(&mut hal, PORT) };
        let channels = hal.constant_config.chan_num_total as usize;
        let handles: Box<[*mut ffi::usb_dwc_hal_chan_t]> = (0..channels).map(|_| ptr::null_mut()).collect();
        hal.channels.hdls = Box::leak(handles).as_mut_ptr();
        let line = cache::alignment(DMA_INTERNAL);
        let frame_len = ffi::usb_hal_frame_list_len_t_USB_HAL_FRAME_LIST_LEN_32;
        let frame_bytes = (frame_len as usize * 4).next_multiple_of(line);
        let frame_list = Self::alloc_dma(frame_bytes, ffi::USB_DWC_FRAME_LIST_MEM_ALIGN as usize).cast::<u32>();
        if frame_list.is_null() {
            return Err(Error::NoMemory);
        }
        // The non-periodic TX FIFO takes a 512-byte bulk packet and the
        // periodic one a 640-byte isochronous OUT packet; IN gets the rest,
        // which has to keep up with three 1024-byte transactions in one
        // microframe.
        let depth = if hal.constant_config.hsphy_type != 0 { 1024 } else { 256 };
        let mut fifo: ffi::usb_dwc_hal_fifo_config_t = unsafe { core::mem::zeroed() };
        fifo.nptx_fifo_lines = depth / 8;
        fifo.ptx_fifo_lines = depth * 5 / 32;
        fifo.rx_fifo_lines = hal.constant_config.fifo_size - fifo.nptx_fifo_lines - fifo.ptx_fifo_lines;

        let inner = Inner {
            hal,
            frame_list,
            frame_bytes,
            fifo,
            events: Some(events),
            speed: Speed::Full,
            enabled: false,
            resetting: false,
            pipes: (0..channels).map(|_| None).collect(),
            done: Queue::new(),
            retired: Queue::new(),
            port_events: [None; 4],
        };
        Self::sync_frame_list(&inner);
        let irq = unsafe { ffi::usb_dwc_info.controllers[PORT as usize].irq };
        let flags = intr::Flags::LEVEL1 | intr::Flags::INTRDISABLED;
        let arg = (self as *const Self).cast_mut().cast();
        let intr = match unsafe { Interrupt::new(irq, flags, Self::isr, arg) } {
            Ok(intr) => intr,
            Err(err) => {
                error!(TAG, "esp_intr_alloc: {err}");
                return Err(Error::Failed);
            }
        };
        let _ = self.intr.set(intr);
        self.inner.with(|slot| *slot = Some(inner));
        self.set_interrupt(true);
        thread::sleep(INIT_DELAY);
        self.locked(Self::power_on);
        Ok(())
    }

    async fn debounce(&self) -> bool {
        executor::sleep(DEBOUNCE).await;
        self.locked(|inner| unsafe {
            let connected = ffi::usb_dwc_hal_port_check_if_connected(&mut inner.hal);
            ffi::usb_dwc_hal_disable_debounce_lock(&mut inner.hal);
            connected
        })
    }

    async fn reset(&self) -> Result<Speed> {
        self.locked(|inner| {
            inner.resetting = true;
            unsafe { ffi::usb_dwc_hal_port_toggle_reset(&mut inner.hal, true) };
        });
        executor::sleep(RESET_HOLD).await;
        self.locked(|inner| unsafe { ffi::usb_dwc_hal_port_toggle_reset(&mut inner.hal, false) });
        executor::sleep(RESET_RECOVERY).await;
        self.locked(|inner| {
            inner.resetting = false;
            if !inner.enabled {
                return Err(Error::NoDevice);
            }
            unsafe {
                ffi::usb_dwc_hal_set_fifo_config(&mut inner.hal, &inner.fifo);
                ffi::usb_dwc_hal_port_set_frame_list(
                    &mut inner.hal,
                    inner.frame_list,
                    ffi::usb_hal_frame_list_len_t_USB_HAL_FRAME_LIST_LEN_32,
                );
                ffi::usb_dwc_hal_port_periodic_enable(&mut inner.hal);
            }
            Ok(inner.speed)
        })
    }

    fn pipe_alloc(&self, config: &PipeConfig) -> Result<PipeId> {
        let control = match config.kind {
            TransferType::Control => true,
            TransferType::Bulk => false,
            _ => return Err(Error::Unsupported),
        };
        self.reap();
        let line = cache::alignment(DMA_INTERNAL);
        let qtd_len = if control { CTRL_QTDS } else { BULK_QTDS };
        let qtd_bytes = (qtd_len * size_of::<ffi::usb_dwc_ll_dma_qtd_t>()).next_multiple_of(line);
        let qtds = Self::alloc_dma(qtd_bytes, qtd_bytes.max(ffi::USB_DWC_QTD_LIST_MEM_ALIGN as usize))
            .cast::<ffi::usb_dwc_ll_dma_qtd_t>();
        let setup = if control { Self::alloc_dma(line, line) } else { ptr::null_mut() };
        if qtds.is_null() || (control && setup.is_null()) {
            unsafe {
                ffi::heap_caps_free(qtds.cast());
                ffi::heap_caps_free(setup.cast());
            }
            return Err(Error::NoMemory);
        }
        // The CPU goes through the uncached alias from here on, so the
        // controller and the CPU never fight over a cache line.
        unsafe {
            msync(
                qtds.cast(),
                qtd_bytes,
                Flags::C2M | Flags::INVALIDATE,
            );
            if control {
                msync(setup, line, Flags::C2M | Flags::INVALIDATE);
            }
        }
        let mut ep_char: ffi::usb_dwc_hal_ep_char_t = unsafe { core::mem::zeroed() };
        unsafe {
            let bits = &mut ep_char.__bindgen_anon_1.__bindgen_anon_1;
            bits.set_type(if control {
                ffi::usb_dwc_xfer_type_t_USB_DWC_XFER_TYPE_CTRL
            } else {
                ffi::usb_dwc_xfer_type_t_USB_DWC_XFER_TYPE_BULK
            });
            bits.set_bEndpointAddress(u32::from(config.endpoint));
            bits.set_mps(u32::from(config.max_packet_bytes));
            bits.set_dev_addr(u32::from(config.address));
        }
        let mut pipe = Box::new(Pipe {
            chan: unsafe { core::mem::zeroed() },
            ep_char,
            has_chan: false,
            control,
            data_in: config.endpoint & 0x80 != 0,
            endpoint: config.endpoint,
            mps: config.max_packet_bytes,
            qtds,
            qtds_nc: cache::uncached(qtds),
            qtd_len,
            setup,
            setup_nc: if control { cache::uncached(setup) } else { ptr::null_mut() },
            pending: Queue::new(),
            active: None,
            state: State::Active,
            ctrl_stage: 0,
            ctrl_in: false,
            ctrl_skip: false,
            ctrl_length: 0,
            halted: AtomicBool::new(false),
        });
        let placed = self.locked(|inner| {
            if !inner.enabled {
                return Err((Error::NoDevice, pipe));
            }
            let Some(index) = inner.pipes.iter().position(Option::is_none) else {
                return Err((Error::Busy, pipe));
            };
            let ok = unsafe { ffi::usb_dwc_hal_chan_alloc(&mut inner.hal, &mut pipe.chan, index as *mut c_void) };
            if !ok {
                return Err((Error::Busy, pipe));
            }
            unsafe { ffi::usb_dwc_hal_chan_set_ep_char(&mut inner.hal, &mut pipe.chan, &mut pipe.ep_char) };
            pipe.has_chan = true;
            inner.pipes[index] = Some(pipe);
            Ok(index)
        });
        match placed {
            Ok(index) => {
                self.locked(|inner| Self::sync_frame_list(inner));
                Ok(PipeId(index as u16))
            }
            Err((err, pipe)) => {
                Self::free_pipe_memory(&pipe);
                Err(err)
            }
        }
    }

    fn pipe_update(&self, pipe: PipeId, address: u8, max_packet_bytes: u16) {
        self.locked(|inner| {
            let Some(p) = inner.pipes[usize::from(pipe.0)].as_mut() else { return };
            unsafe {
                let bits = &mut p.ep_char.__bindgen_anon_1.__bindgen_anon_1;
                bits.set_dev_addr(u32::from(address));
                bits.set_mps(u32::from(max_packet_bytes));
            }
            p.mps = max_packet_bytes;
            if p.has_chan && !p.chan_active() {
                unsafe { ffi::usb_dwc_hal_chan_set_ep_char(&mut inner.hal, &mut p.chan, &mut p.ep_char) };
            }
        });
    }

    fn pipe_free(&self, pipe: PipeId) {
        if !self.initialized() {
            return;
        }
        let index = usize::from(pipe.0);
        self.halt_and_wait(index);
        let freed = self.locked(|inner| {
            cancel_all(inner, index, Error::Canceled);
            let mut p = inner.pipes[index].take()?;
            if p.has_chan {
                p.clear_chan_flags(CHAN_FLAG_ACTIVE);
                unsafe { ffi::usb_dwc_hal_chan_free(&mut inner.hal, &mut p.chan) };
                p.has_chan = false;
            }
            Some(p)
        });
        if let Some(p) = freed {
            self.locked(|inner| Self::sync_frame_list(inner));
            Self::free_pipe_memory(&p);
        }
        self.reap();
    }

    fn submit(&self, pipe: PipeId, request: Arc<Request>) -> Result<()> {
        self.reap();
        let index = usize::from(pipe.0);
        let (control, data_in) = self.locked(|inner| {
            inner.pipes[index].as_ref().map(|p| (p.control, p.data_in)).ok_or(Error::NoDevice)
        })?;
        if !control && request.len > MAX_BULK_BYTES {
            return Err(Error::InvalidArgument);
        }
        let (data, capacity) = unsafe { request.dma() };
        let reads = if control { request.is_in() } else { data_in };
        unsafe {
            if reads {
                msync(data, capacity, Flags::M2C);
            } else {
                msync(data, request.len, Flags::C2M | Flags::UNALIGNED);
            }
        }
        self.locked(|inner| {
            let p = inner.pipes[index].as_mut().ok_or(Error::NoDevice)?;
            if p.state == State::Dead {
                return Err(Error::NoDevice);
            }
            p.pending.push(request);
            Self::start(inner, index);
            Ok(())
        })
    }

    fn cancel(&self, pipe: PipeId, request: &Arc<Request>) {
        let index = usize::from(pipe.0);
        self.locked(|inner| {
            let Some(p) = inner.pipes[index].as_mut() else { return };
            if let Some(r) = p.pending.remove(request) {
                finish(inner, r, Err(Error::Canceled));
                return;
            }
            if !p.active.as_ref().is_some_and(|a| Arc::ptr_eq(a, request)) || p.state != State::Active {
                return;
            }
            if p.chan_active() && !unsafe { ffi::usb_dwc_hal_chan_request_halt(&mut p.chan) } {
                p.state = State::Canceling;
            } else {
                Self::finish_active(inner, index, Err(Error::Canceled));
                Self::start(inner, index);
            }
        });
    }

    fn clear(&self, pipe: PipeId) {
        let index = usize::from(pipe.0);
        self.locked(|inner| {
            let Some(p) = inner.pipes[index].as_mut() else { return };
            if matches!(p.state, State::Dead | State::Halting | State::Canceling) {
                return;
            }
            p.state = State::Active;
            if !p.control && p.has_chan {
                unsafe { ffi::usb_dwc_hal_chan_set_pid(&mut p.chan, 0) };
            }
            Self::start(inner, index);
        });
    }

    fn recover(&self) {
        self.locked(|inner| {
            for index in 0..inner.pipes.len() {
                cancel_all(inner, index, Error::NoDevice);
                if let Some(p) = inner.pipes[index].as_mut() {
                    p.state = State::Dead;
                    p.clear_chan_flags(CHAN_FLAG_ACTIVE);
                    p.has_chan = false;
                }
            }
            inner.enabled = false;
        });
        self.set_interrupt(false);
        self.locked(|inner| {
            unsafe {
                ffi::usb_dwc_hal_core_soft_reset(&mut inner.hal);
                ptr::write_bytes(inner.frame_list, 0, inner.frame_bytes / 4);
            }
            Self::sync_frame_list(inner);
        });
        self.set_interrupt(true);
        self.locked(Self::power_on);
        self.reap();
    }

    fn finished(&self, request: &Request) {
        if request.is_in() {
            let (data, capacity) = unsafe { request.dma() };
            unsafe { msync(data, capacity, Flags::M2C) };
        }
        self.reap();
    }
}
