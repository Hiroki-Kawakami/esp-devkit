/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::{CStr, c_void};
use core::mem;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use idf::heap::{self, Caps};
use idf::log::error;
use idf::spi::Bus;
use idf::sync::{Mutex, Semaphore};
use idf::{Error, Result, sys};

use crate::Absent;
use crate::display::{Driver, GramDriver, Image, ImageFormat, Rect, Rotation, Rotations, Size, Ticket};

const TAG: &CStr = c"spi_lcd";

const QUEUE_DEPTH: usize = 32;
// Pixels DMA cannot read where they are (PSRAM, flash) are copied through
// these, as reading PSRAM directly falls behind a quad link.
const BOUNCE_BYTES: usize = 4096;
const MAX_WAKES: u32 = 64;
const MADCTL_MY: u8 = 0x80;
const MADCTL_MX: u8 = 0x40;
const MADCTL_MV: u8 = 0x20;
const QUAD_COMMAND: u16 = 0x02;
const QUAD_PIXELS: u16 = 0x32;
const QUAD_COMMAND_BITS: u8 = 8;
const QUAD_ADDRESS_BITS: u8 = 24;

pub(crate) type Backlight = Box<dyn Fn(u8) -> Result<()> + Send + Sync>;

#[allow(dead_code)]
pub(crate) enum Link {
    /// One data line, with a line telling commands from data.
    Dc(i32),
    /// Commands and their parameters on one line, pixels on four.
    Quad,
}

#[allow(dead_code)]
pub(crate) enum Brightness {
    Fixed,
    /// 0..=100.
    Backlight(Backlight),
    /// The panel's own, set by WRDISBV.
    Dcs,
}

pub(crate) struct Config {
    pub bus: Bus,
    pub cs: i32,
    pub link: Link,
    pub pclk_hz: u32,
    /// Unturned.
    pub size: Size,
    /// The panel's memory, unturned; the picture is `size` of it from `offset`.
    pub memory: Size,
    pub offset: (u16, u16),
    /// Of the windows written, in panel memory, along both axes.
    pub alignment: u16,
    pub ppi: u16,
    pub pixel_format: ImageFormat,
    /// For the unturned picture; a rotation flips its MV, MX and MY bits.
    pub madctl: u8,
    /// Whether the panel honours MADCTL MV, which a quarter turn needs.
    pub exchanges: bool,
    pub rotation: Rotation,
    /// Everything up to the picture showing, but MADCTL.
    pub init: fn(&Io) -> Result<()>,
    pub brightness: Brightness,
}

/// A transaction and what the bus interrupt does around it; `base` first, so
/// the driver's pointer is one to this.
#[repr(C)]
struct Trans {
    base: sys::spi_transaction_t,
    data: bool,
    last: bool,
}

/// What goes ahead of a transaction's bytes.
#[derive(Clone, Copy)]
struct Head {
    data: bool,
    cmd: u16,
    addr: u64,
    flags: u32,
}

impl Head {
    const fn dc(data: bool) -> Self {
        Self { data, cmd: 0, addr: 0, flags: 0 }
    }

    const fn quad(instruction: u16, dcs: u8, flags: u32) -> Self {
        Self { data: true, cmd: instruction, addr: (dcs as u64) << 8, flags }
    }
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
    /// Transactions queued and reclaimed so far; they finish in order.
    queued: u32,
    reclaimed: u32,
    bounce: [Box<[u8], Caps>; 2],
    /// The transaction last sending each bounce buffer.
    bounce_sent: [Option<u32>; 2],
    bounce_turn: usize,
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
    quad: bool,
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
    fn new(bus: &Bus, cs: i32, link: &Link, pclk_hz: u32) -> Result<Self> {
        let mut config: sys::spi_device_interface_config_t = unsafe { mem::zeroed() };
        config.flags = sys::SPI_DEVICE_HALFDUPLEX;
        config.clock_speed_hz = pclk_hz as i32;
        config.spics_io_num = cs;
        config.queue_size = QUEUE_DEPTH as i32;
        config.post_cb = Some(after);
        let dc = match *link {
            Link::Dc(dc) => {
                Error::check(unsafe { sys::gpio_reset_pin(dc) })?;
                Error::check(unsafe { sys::gpio_set_direction(dc, sys::gpio_mode_t_GPIO_MODE_OUTPUT) })?;
                config.pre_cb = Some(before);
                dc
            }
            Link::Quad => {
                config.command_bits = QUAD_COMMAND_BITS;
                config.address_bits = QUAD_ADDRESS_BITS;
                -1
            }
        };
        let shared = Box::new(Shared { dc, completed: AtomicU32::new(0), waiters: AtomicU32::new(0), wake: Semaphore::try_new(0, MAX_WAKES)? });
        let mut device = ptr::null_mut();
        Error::check(unsafe { sys::spi_bus_add_device(bus.host() as sys::spi_host_device_t, &config, &mut device) })?;
        let mut max_bytes = 0;
        unsafe { sys::spi_bus_get_max_transaction_len(bus.host() as sys::spi_host_device_t, &mut max_bytes) };
        let pool = (0..QUEUE_DEPTH).map(|_| Trans { base: unsafe { mem::zeroed() }, data: false, last: false }).collect();
        let bounce_caps = Caps::DMA | Caps::INTERNAL;
        let bounce = [heap::zeroed_slice(BOUNCE_BYTES, bounce_caps)?, heap::zeroed_slice(BOUNCE_BYTES, bounce_caps)?];
        let queue = Mutex::new(Queue {
            pool,
            next: 0,
            inflight: 0,
            issued: 0,
            queued: 0,
            reclaimed: 0,
            bounce,
            bounce_sent: [None; 2],
            bounce_turn: 0,
        });
        let quad = matches!(link, Link::Quad);
        Ok(Self { device, quad, max_bytes: max_bytes.max(1), shared, queue, _bus: bus.clone() })
    }

    fn reclaim(&self, queue: &mut Queue) -> Result<()> {
        let mut done = ptr::null_mut();
        Error::check(unsafe { sys::spi_device_get_trans_result(self.device, &mut done, u32::MAX) })?;
        queue.inflight -= 1;
        queue.reclaimed = queue.reclaimed.wrapping_add(1);
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

    fn reap(&self, queue: &mut Queue) {
        while queue.inflight > 0 {
            let mut done = ptr::null_mut();
            if Error::check(unsafe { sys::spi_device_get_trans_result(self.device, &mut done, 0) }).is_err() {
                return;
            }
            queue.inflight -= 1;
            queue.reclaimed = queue.reclaimed.wrapping_add(1);
        }
    }

    fn push(&self, queue: &mut Queue, head: Head, bytes: Bytes<'_>, last: bool) -> Result<()> {
        self.reap(queue);
        if queue.inflight == queue.pool.len() {
            self.reclaim(queue)?;
        }
        let trans = &mut queue.pool[queue.next];
        trans.base = unsafe { mem::zeroed() };
        trans.base.user = (&*self.shared as *const Shared).cast_mut().cast::<c_void>();
        trans.base.flags = head.flags;
        trans.base.cmd = head.cmd;
        trans.base.addr = head.addr;
        (trans.data, trans.last) = (head.data, last);
        match bytes {
            Bytes::Inline([]) => {}
            Bytes::Inline(b) => {
                trans.base.flags |= sys::SPI_TRANS_USE_TXDATA;
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
        queue.queued = queue.queued.wrapping_add(1);
        Ok(())
    }

    /// Sends `chunk` out of the next bounce buffer, once what it held before
    /// has gone.
    fn pixels_bounced(&self, queue: &mut Queue, cmd: u32, chunk: &[u8], last: bool) -> Result<()> {
        let turn = queue.bounce_turn;
        if let Some(sent) = queue.bounce_sent[turn] {
            while queue.reclaimed.wrapping_sub(sent) as i32 <= 0 {
                self.reclaim(queue)?;
            }
        }
        let buffer = &mut queue.bounce[turn][..chunk.len()];
        buffer.copy_from_slice(chunk);
        let buffer = unsafe { core::slice::from_raw_parts(buffer.as_ptr(), buffer.len()) };
        self.pixels(queue, cmd, buffer, last)?;
        queue.bounce_sent[turn] = Some(queue.queued.wrapping_sub(1));
        queue.bounce_turn = 1 - turn;
        Ok(())
    }

    fn command(&self, queue: &mut Queue, cmd: u32, params: &[u8]) -> Result<()> {
        let params = if params.len() <= 4 { Bytes::Inline(params) } else { Bytes::Borrowed(params) };
        if self.quad {
            return self.push(queue, Head::quad(QUAD_COMMAND, cmd as u8, 0), params, false);
        }
        self.push(queue, Head::dc(false), Bytes::Inline(&[cmd as u8]), false)?;
        match params {
            Bytes::Inline([]) => Ok(()),
            params => self.push(queue, Head::dc(true), params, false),
        }
    }

    fn pixels(&self, queue: &mut Queue, cmd: u32, chunk: &[u8], last: bool) -> Result<()> {
        if self.quad {
            return self.push(queue, Head::quad(QUAD_PIXELS, cmd as u8, sys::SPI_TRANS_MODE_QIO), Bytes::Borrowed(chunk), last);
        }
        self.push(queue, Head::dc(false), Bytes::Inline(&[cmd as u8]), false)?;
        self.push(queue, Head::dc(true), Bytes::Borrowed(chunk), last)
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
            let direct = heap::dma_capable(row.as_ptr());
            let chunks = row.chunks(if direct { self.max_bytes } else { BOUNCE_BYTES.min(self.max_bytes) });
            let pieces = chunks.len();
            for (k, chunk) in chunks.enumerate() {
                let cmd = if first { sys::LCD_CMD_RAMWR } else { sys::LCD_CMD_RAMWRC };
                first = false;
                let last = j + 1 == count && k + 1 == pieces;
                if direct {
                    self.pixels(&mut queue, cmd, chunk, last)?;
                } else {
                    self.pixels_bounced(&mut queue, cmd, chunk, last)?;
                }
            }
        }
        queue.issued = queue.issued.wrapping_add(1);
        Ok(Ticket(queue.issued))
    }

    fn wait(&self, ticket: Ticket) {
        let shared = &*self.shared;
        if !shared.reached(ticket.0) {
            shared.waiters.fetch_add(1, Ordering::SeqCst);
            while !shared.reached(ticket.0) {
                shared.wake.take();
            }
            shared.waiters.fetch_sub(1, Ordering::SeqCst);
        }
        self.reap(&mut self.queue.lock());
    }
}

/// A panel that keeps the picture in its own memory, written with the MIPI DCS
/// commands over SPI.
pub(crate) struct SpiLcd {
    io: Io,
    size: Size,
    memory: Size,
    offset: (u16, u16),
    alignment: u16,
    ppi: u16,
    format: ImageFormat,
    madctl: u8,
    exchanges: bool,
    rotation: Rotation,
    /// Where the turned picture starts in the addresses written.
    origin: (i16, i16),
    brightness: Brightness,
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

// MX and MY reverse the memory's columns and rows, and MV then exchanges them
// for the addresses written.
fn origin(madctl: u8, size: Size, memory: Size, (x, y): (u16, u16)) -> (i16, i16) {
    let x = if madctl & MADCTL_MX != 0 { memory.width - x - size.width } else { x } as i16;
    let y = if madctl & MADCTL_MY != 0 { memory.height - y - size.height } else { y } as i16;
    if madctl & MADCTL_MV != 0 { (y, x) } else { (x, y) }
}

impl SpiLcd {
    pub(crate) fn new(config: Config) -> Result<Self> {
        let io = Io::new(&config.bus, config.cs, &config.link, config.pclk_hz).inspect_err(|err| error!(TAG, "panel io: {err}"))?;
        (config.init)(&io).inspect_err(|err| error!(TAG, "init: {err}"))?;
        let mut lcd = Self {
            io,
            size: config.size,
            memory: config.memory,
            offset: config.offset,
            alignment: config.alignment,
            ppi: config.ppi,
            format: config.pixel_format,
            madctl: config.madctl,
            exchanges: config.exchanges,
            rotation: Rotation::R0,
            origin: (0, 0),
            brightness: config.brightness,
        };
        lcd.turn(config.rotation)?;
        Ok(lcd)
    }

    fn turn(&mut self, rotation: Rotation) -> Result<()> {
        if rotation.swaps_axes() && !self.exchanges {
            error!(TAG, "rotation {rotation:?}: the panel turns only by halves");
            return Err(Error::NOT_SUPPORTED);
        }
        let madctl = self.madctl ^ turn_bits(rotation);
        let origin = origin(madctl, self.size, self.memory, self.offset);
        let align = self.alignment as i16;
        if origin.0 % align != 0 || origin.1 % align != 0 {
            error!(TAG, "rotation {rotation:?}: the picture starts at {origin:?}, off the {align}-pixel grid");
            return Err(Error::INVALID_ARG);
        }
        self.io
            .tx_param(sys::LCD_CMD_MADCTL as i32, &[madctl])
            .inspect_err(|err| error!(TAG, "rotation {rotation:?}: {err}"))?;
        self.rotation = rotation;
        self.origin = origin;
        Ok(())
    }

    fn place(&self, area: Rect) -> Rect {
        area.offset(self.origin.0, self.origin.1)
    }
}

impl Driver for SpiLcd {
    type FrameBuffers = Absent;
    type Gram = Self;
    type Epd = Absent;

    fn size(&self) -> Size {
        if self.rotation.swaps_axes() { Size::new(self.size.height, self.size.width) } else { self.size }
    }

    fn ppi(&self) -> u16 {
        self.ppi
    }

    fn pixel_format(&self) -> ImageFormat {
        self.format
    }

    fn rotation(&self) -> Rotation {
        self.rotation
    }

    fn rotations(&self) -> Rotations {
        if self.exchanges { Rotations::all() } else { Rotations::R0.union(Rotations::R180) }
    }

    fn alignment(&self) -> u16 {
        self.alignment
    }

    fn gram(&self) -> Option<&Self> {
        Some(self)
    }

    fn draw(&self, area: Rect, image: &Image) -> Result<()> {
        let row = self.format.row_bytes(area.w as usize);
        let stride = image.stride_bytes();
        let rows = area.h as usize;
        let area = self.place(area);
        let ticket = if stride == row {
            unsafe { self.io.write_rows(area, [&image.pixels()[..row * rows]].into_iter()) }?
        } else {
            unsafe { self.io.write_rows(area, (0..rows).map(|j| &image.pixels()[j * stride..][..row])) }?
        };
        self.io.wait(ticket);
        Ok(())
    }

    fn set_brightness(&self, level: u8) -> Result<()> {
        let level = level.min(100);
        match &self.brightness {
            Brightness::Fixed => Err(Error::NOT_SUPPORTED),
            Brightness::Backlight(backlight) => backlight(level),
            Brightness::Dcs => self.io.tx_param(sys::LCD_CMD_WRDISBV as i32, &[(u16::from(level) * 255 / 100) as u8]),
        }
    }

    fn reconfigure(&mut self, format: ImageFormat, rotation: Rotation) -> Result<()> {
        if format != self.format {
            return Err(Error::NOT_SUPPORTED);
        }
        if rotation == self.rotation {
            return Ok(());
        }
        self.turn(rotation)
    }
}

impl GramDriver for SpiLcd {
    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        unsafe { self.io.write_rows(self.place(area), [pixels].into_iter()) }
    }

    fn wait(&self, ticket: Ticket) -> Result<()> {
        self.io.wait(ticket);
        Ok(())
    }
}
