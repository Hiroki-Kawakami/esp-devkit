/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::{CStr, c_void};
use core::mem;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use idf::log::error;
use idf::spi::Bus;
use idf::sync::{Mutex, Semaphore};
use idf::{Error, Result, sys};

use crate::display::{Driver, Image, ImageFormat, Rect, Rotation, Size, Ticket};

const TAG: &CStr = c"spi_lcd";

const QUEUE_DEPTH: usize = 32;
const MAX_WAKES: u32 = 64;
const MADCTL_MY: u8 = 0x80;
const MADCTL_MX: u8 = 0x40;
const MADCTL_MV: u8 = 0x20;

pub(crate) type Backlight = Box<dyn Fn(u8) -> Result<()> + Send + Sync>;

pub(crate) struct Config {
    pub bus: Bus,
    pub cs: i32,
    pub dc: i32,
    pub pclk_hz: u32,
    /// Unturned.
    pub size: Size,
    pub pixel_format: ImageFormat,
    /// For the unturned picture; a rotation flips its MV, MX and MY bits.
    pub madctl: u8,
    pub rotation: Rotation,
    /// Everything up to the picture showing, but MADCTL.
    pub init: fn(&Io) -> Result<()>,
    /// 0..=100.
    pub backlight: Option<Backlight>,
}

/// A transaction and what the bus interrupt does around it; `base` first, so
/// the driver's pointer is one to this.
#[repr(C)]
struct Trans {
    base: sys::spi_transaction_t,
    data: bool,
    last: bool,
}

/// Read from the bus interrupt.
struct Shared {
    dc: i32,
    completed: AtomicU32,
    waiters: AtomicU32,
    wake: Semaphore,
}

impl Shared {
    fn reached(&self, ticket: u32) -> bool {
        self.completed.load(Ordering::SeqCst).wrapping_sub(ticket) as i32 >= 0
    }
}

// The bus interrupt is not allocated IRAM-safe, so these may stay in flash.
unsafe extern "C" fn before(trans: *mut sys::spi_transaction_t) {
    let trans = unsafe { &*trans.cast::<Trans>() };
    let shared = unsafe { &*trans.base.user.cast::<Shared>() };
    unsafe { sys::gpio_set_level(shared.dc, u32::from(trans.data)) };
}

unsafe extern "C" fn after(trans: *mut sys::spi_transaction_t) {
    let trans = unsafe { &*trans.cast::<Trans>() };
    if !trans.last {
        return;
    }
    let shared = unsafe { &*trans.base.user.cast::<Shared>() };
    shared.completed.fetch_add(1, Ordering::SeqCst);
    for _ in 0..shared.waiters.load(Ordering::SeqCst) {
        shared.wake.give_from_isr();
    }
}

struct Queue {
    pool: Box<[Trans]>,
    next: usize,
    inflight: usize,
    issued: u32,
}

unsafe impl Send for Queue {}

/// What one transaction sends.
enum Bytes<'a> {
    Inline(&'a [u8]),
    /// Must stay unchanged until the transaction is done.
    Borrowed(&'a [u8]),
}

/// MIPI DBI commands and pixels over SPI, queued to the bus without waiting
/// for what went before, and done in the order they were queued.
pub(crate) struct Io {
    device: sys::spi_device_handle_t,
    max_bytes: usize,
    shared: Box<Shared>,
    queue: Mutex<Queue>,
    _bus: Bus,
}

unsafe impl Send for Io {}
unsafe impl Sync for Io {}

impl Drop for Io {
    fn drop(&mut self) {
        let mut queue = self.queue.lock();
        self.drain(&mut queue);
        unsafe { sys::spi_bus_remove_device(self.device) };
    }
}

impl Io {
    fn new(bus: &Bus, cs: i32, dc: i32, pclk_hz: u32) -> Result<Self> {
        Error::check(unsafe { sys::gpio_reset_pin(dc) })?;
        Error::check(unsafe { sys::gpio_set_direction(dc, sys::gpio_mode_t_GPIO_MODE_OUTPUT) })?;
        let shared = Box::new(Shared { dc, completed: AtomicU32::new(0), waiters: AtomicU32::new(0), wake: Semaphore::try_new(0, MAX_WAKES)? });
        let mut config: sys::spi_device_interface_config_t = unsafe { mem::zeroed() };
        config.flags = sys::SPI_DEVICE_HALFDUPLEX;
        config.clock_speed_hz = pclk_hz as i32;
        config.spics_io_num = cs;
        config.queue_size = QUEUE_DEPTH as i32;
        config.pre_cb = Some(before);
        config.post_cb = Some(after);
        let mut device = ptr::null_mut();
        Error::check(unsafe { sys::spi_bus_add_device(bus.host() as sys::spi_host_device_t, &config, &mut device) })?;
        let mut max_bytes = 0;
        unsafe { sys::spi_bus_get_max_transaction_len(bus.host() as sys::spi_host_device_t, &mut max_bytes) };
        let pool = (0..QUEUE_DEPTH).map(|_| Trans { base: unsafe { mem::zeroed() }, data: false, last: false }).collect();
        let queue = Mutex::new(Queue { pool, next: 0, inflight: 0, issued: 0 });
        Ok(Self { device, max_bytes: max_bytes.max(1), shared, queue, _bus: bus.clone() })
    }

    fn reclaim(&self, queue: &mut Queue) -> Result<()> {
        let mut done = ptr::null_mut();
        Error::check(unsafe { sys::spi_device_get_trans_result(self.device, &mut done, u32::MAX) })?;
        queue.inflight -= 1;
        Ok(())
    }

    fn drain(&self, queue: &mut Queue) {
        while queue.inflight > 0 {
            if let Err(err) = self.reclaim(queue) {
                error!(TAG, "reclaim: {err}");
                return;
            }
        }
    }

    fn push(&self, queue: &mut Queue, data: bool, bytes: Bytes<'_>, last: bool) -> Result<()> {
        if queue.inflight == queue.pool.len() {
            self.reclaim(queue)?;
        }
        let trans = &mut queue.pool[queue.next];
        trans.base = unsafe { mem::zeroed() };
        trans.base.user = (&*self.shared as *const Shared).cast_mut().cast::<c_void>();
        (trans.data, trans.last) = (data, last);
        match bytes {
            Bytes::Inline(b) => {
                trans.base.flags = sys::SPI_TRANS_USE_TXDATA;
                trans.base.length = b.len() * 8;
                unsafe { trans.base.__bindgen_anon_1.tx_data[..b.len()].copy_from_slice(b) };
            }
            Bytes::Borrowed(b) => {
                trans.base.length = b.len() * 8;
                trans.base.__bindgen_anon_1.tx_buffer = b.as_ptr().cast();
            }
        }
        Error::check(unsafe { sys::spi_device_queue_trans(self.device, &mut trans.base, u32::MAX) })?;
        queue.next = (queue.next + 1) % queue.pool.len();
        queue.inflight += 1;
        Ok(())
    }

    fn command(&self, queue: &mut Queue, cmd: u32, params: &[u8]) -> Result<()> {
        self.push(queue, false, Bytes::Inline(&[cmd as u8]), false)?;
        match params.len() {
            0 => Ok(()),
            1..=4 => self.push(queue, true, Bytes::Inline(params), false),
            _ => self.push(queue, true, Bytes::Borrowed(params), false),
        }
    }

    /// Sends `cmd` with `param` and waits for it and everything before.
    pub(crate) fn tx_param(&self, cmd: i32, param: &[u8]) -> Result<()> {
        let mut queue = self.queue.lock();
        let sent = self.command(&mut queue, cmd as u32, param);
        self.drain(&mut queue);
        sent
    }

    /// Queues `rows` into `area`, whose rows they are. Each row must stay
    /// unchanged until `wait` for the ticket returns.
    unsafe fn write_rows<'a>(&self, area: Rect, rows: impl ExactSizeIterator<Item = &'a [u8]>) -> Result<Ticket> {
        let mut queue = self.queue.lock();
        self.command(&mut queue, sys::LCD_CMD_CASET, &span(area.x, area.w))?;
        self.command(&mut queue, sys::LCD_CMD_RASET, &span(area.y, area.h))?;
        let count = rows.len();
        let mut first = true;
        for (j, row) in rows.enumerate() {
            let chunks = row.chunks(self.max_bytes);
            let pieces = chunks.len();
            for (k, chunk) in chunks.enumerate() {
                let cmd = if first { sys::LCD_CMD_RAMWR } else { sys::LCD_CMD_RAMWRC };
                first = false;
                self.push(&mut queue, false, Bytes::Inline(&[cmd as u8]), false)?;
                self.push(&mut queue, true, Bytes::Borrowed(chunk), j + 1 == count && k + 1 == pieces)?;
            }
        }
        queue.issued = queue.issued.wrapping_add(1);
        Ok(Ticket(queue.issued))
    }

    fn wait(&self, ticket: Ticket) {
        let shared = &*self.shared;
        if shared.reached(ticket.0) {
            return;
        }
        shared.waiters.fetch_add(1, Ordering::SeqCst);
        while !shared.reached(ticket.0) {
            shared.wake.take();
        }
        shared.waiters.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A panel that keeps the picture in its own memory, written with the MIPI DCS
/// commands over SPI.
pub(crate) struct SpiLcd {
    io: Io,
    size: Size,
    format: ImageFormat,
    madctl: u8,
    rotation: Rotation,
    backlight: Option<Backlight>,
}

fn turn_bits(rotation: Rotation) -> u8 {
    match rotation {
        Rotation::R0 => 0,
        Rotation::R90 => MADCTL_MV | MADCTL_MY,
        Rotation::R180 => MADCTL_MX | MADCTL_MY,
        Rotation::R270 => MADCTL_MV | MADCTL_MX,
    }
}

fn span(start: i16, len: i16) -> [u8; 4] {
    let (start, end) = (start as u16, (start + len - 1) as u16);
    let (s, e) = (start.to_be_bytes(), end.to_be_bytes());
    [s[0], s[1], e[0], e[1]]
}

impl SpiLcd {
    pub(crate) fn new(config: Config) -> Result<Self> {
        let io = Io::new(&config.bus, config.cs, config.dc, config.pclk_hz).inspect_err(|err| error!(TAG, "panel io: {err}"))?;
        (config.init)(&io).inspect_err(|err| error!(TAG, "init: {err}"))?;
        let mut lcd = Self {
            io,
            size: config.size,
            format: config.pixel_format,
            madctl: config.madctl,
            rotation: Rotation::R0,
            backlight: config.backlight,
        };
        lcd.turn(config.rotation)?;
        Ok(lcd)
    }

    fn turn(&mut self, rotation: Rotation) -> Result<()> {
        let madctl = self.madctl ^ turn_bits(rotation);
        self.io
            .tx_param(sys::LCD_CMD_MADCTL as i32, &[madctl])
            .inspect_err(|err| error!(TAG, "rotation {rotation:?}: {err}"))?;
        self.rotation = rotation;
        Ok(())
    }
}

impl Driver for SpiLcd {
    fn size(&self) -> Size {
        if self.rotation.swaps_axes() { Size::new(self.size.height, self.size.width) } else { self.size }
    }

    fn pixel_format(&self) -> ImageFormat {
        self.format
    }

    fn rotation(&self) -> Rotation {
        self.rotation
    }

    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        unsafe { self.io.write_rows(area, [pixels].into_iter()) }
    }

    fn wait(&self, ticket: Ticket) -> Result<()> {
        self.io.wait(ticket);
        Ok(())
    }

    fn draw(&self, area: Rect, image: &Image) -> Result<()> {
        let row = self.format.row_bytes(area.w as usize);
        let stride = image.stride_bytes();
        let rows = area.h as usize;
        let ticket = if stride == row {
            unsafe { self.io.write_rows(area, [&image.pixels()[..row * rows]].into_iter()) }?
        } else {
            unsafe { self.io.write_rows(area, (0..rows).map(|j| &image.pixels()[j * stride..][..row])) }?
        };
        self.io.wait(ticket);
        Ok(())
    }

    fn set_brightness(&self, level: u8) -> Result<()> {
        let backlight = self.backlight.as_ref().ok_or(Error::NOT_SUPPORTED)?;
        backlight(level.min(100))
    }

    fn reconfigure(&mut self, format: ImageFormat, fb_num: u8, rotation: Rotation) -> Result<()> {
        if format != self.format || fb_num != 0 {
            return Err(Error::NOT_SUPPORTED);
        }
        if rotation == self.rotation {
            return Ok(());
        }
        self.turn(rotation)
    }
}
