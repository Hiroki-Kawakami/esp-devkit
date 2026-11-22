/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! IT8951E controller for electrophoretic panels, over SPI: rectangles go
//! into its memory, turned by the chip, and it drives them to the glass.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::CStr;
use core::iter;
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use core::time::Duration;

use idf::heap::{self, Caps};
use idf::log::{error, info, warn};
use idf::spi::{self, Device, DeviceConfig, Transfer};
use idf::sync::Mutex;
use idf::time::{Instant, delay_us};
use idf::{Error, Result, gpio, thread};

use crate::Absent;
use crate::dispatch::{self, Next, Notifier, Source};
use crate::display::{Driver, EpdDriver, EpdMode, GramDriver, Image, ImageFormat, Rect, Rotation, Rotations, Size, Ticket, Waveform};

const TAG: &CStr = c"it8951e";

const PRE_COMMAND: u16 = 0x6000;
const PRE_WRITE: u16 = 0x0000;
const PRE_READ: u16 = 0x1000;

const CMD_SYS_RUN: u16 = 0x0001;
const CMD_STANDBY: u16 = 0x0002;
const CMD_REG_RD: u16 = 0x0010;
const CMD_REG_WR: u16 = 0x0011;
const CMD_LD_IMG_AREA: u16 = 0x0021;
const CMD_LD_IMG_END: u16 = 0x0022;
const CMD_DPY_BUF_AREA: u16 = 0x0037;
const CMD_VCOM: u16 = 0x0039;
const CMD_GET_DEV_INFO: u16 = 0x0302;

const REG_I80CPCR: u16 = 0x0004;
const REG_LISAR: u16 = 0x0208;
const REG_LUTAFSR: u16 = 0x1224;

const PACKED_WRITE: u16 = 0x0001;
const VCOM_GET: u16 = 0;
const VCOM_SET: u16 = 1;
const BIG_ENDIAN: u16 = 1;
const BPP_4: u16 = 2;
const BPP_8: u16 = 3;

const MODE_INIT: u16 = 0;
const MODE_DU: u16 = 1;
const MODE_GC16: u16 = 2;
const MODE_GL16: u16 = 3;

const SPI_MODE: u8 = 0;
const DEV_INFO_WORDS: usize = 20;
const BOUNCE_BYTES: usize = 4096;
const WHITE: [u8; 512] = [0xFF; 512];

const HRDY_TIMEOUT: Duration = Duration::from_secs(3);
// A full flashing drive keeps HRDY low for seconds.
const DRIVE_TIMEOUT: Duration = Duration::from_secs(30);
const HRDY_SPIN: Duration = Duration::from_micros(500);
const HRDY_SPIN_STEP_US: u32 = 10;
const BUSY_POLL: Duration = Duration::from_millis(5);

pub(crate) struct Config {
    pub bus: spi::Bus,
    pub cs: i32,
    pub hrdy: i32,
    pub write_hz: u32,
    /// A second device on the same chip select reads at this clock when it
    /// differs from `write_hz`.
    pub read_hz: u32,
    pub input_delay_ns: i32,
    /// Unturned.
    pub size: Size,
    pub ppi: u16,
    pub vcom_mv: u16,
    /// The image buffer, when the chip's answer cannot be trusted.
    pub fallback_address: u32,
    /// L4 or L8.
    pub pixel_format: ImageFormat,
    pub rotation: Rotation,
    /// The chip stands by after this long without a command.
    pub rest_after: Duration,
}

fn wait_ready(hrdy: i32, timeout: Duration) -> Result<()> {
    if gpio::level(hrdy) {
        return Ok(());
    }
    let start = Instant::now();
    while !gpio::level(hrdy) {
        let waited = start.elapsed();
        if waited > timeout {
            error!(TAG, "HRDY low for {waited:?}");
            return Err(Error::TIMEOUT);
        }
        if waited < HRDY_SPIN {
            delay_us(HRDY_SPIN_STEP_US);
        } else {
            thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

fn word(device: &Device, value: u16) -> Result<()> {
    device.transmit_polling(&value.to_be_bytes())
}

/// The packets of the host interface; the chip select is a GPIO so a slower
/// device for reads can share it.
struct Io {
    write: Device,
    read: Option<Device>,
    cs: i32,
    hrdy: i32,
    bounce: [Box<[u8], Caps>; 2],
    transfers: [Transfer; 2],
}

impl Io {
    fn new(bus: &spi::Bus, cs: i32, hrdy: i32, write_hz: u32, read_hz: u32, input_delay_ns: i32) -> Result<Self> {
        let device = |clock_hz| {
            Device::new(bus, &DeviceConfig { clock_hz, mode: SPI_MODE, cs: None, input_delay_ns, half_duplex: true, queue_size: 2 })
        };
        let write = device(write_hz)?;
        let read = if read_hz == write_hz { None } else { Some(device(read_hz)?) };
        let caps = Caps::DMA | Caps::INTERNAL;
        let bounce = [heap::zeroed_slice(BOUNCE_BYTES, caps)?, heap::zeroed_slice(BOUNCE_BYTES, caps)?];
        Ok(Self { write, read, cs, hrdy, bounce, transfers: [Transfer::default(), Transfer::default()] })
    }

    fn wait_ready(&self, timeout: Duration) -> Result<()> {
        wait_ready(self.hrdy, timeout)
    }

    fn selected<R>(cs: i32, device: &Device, f: impl FnOnce() -> Result<R>) -> Result<R> {
        let _bus = device.acquire()?;
        gpio::set_level(cs, false)?;
        let result = f();
        let _ = gpio::set_level(cs, true);
        result
    }

    fn command(&self, command: u16) -> Result<()> {
        self.wait_ready(HRDY_TIMEOUT)?;
        Self::selected(self.cs, &self.write, || {
            word(&self.write, PRE_COMMAND)?;
            self.wait_ready(HRDY_TIMEOUT)?;
            word(&self.write, command)
        })
    }

    // Every word waits for HRDY: overrunning the chip's input FIFO freezes
    // its display engine.
    fn args(&self, args: &[u16]) -> Result<()> {
        self.wait_ready(HRDY_TIMEOUT)?;
        Self::selected(self.cs, &self.write, || {
            word(&self.write, PRE_WRITE)?;
            for &arg in args {
                self.wait_ready(HRDY_TIMEOUT)?;
                word(&self.write, arg)?;
            }
            Ok(())
        })
    }

    fn command_args(&self, command: u16, args: &[u16]) -> Result<()> {
        self.command(command)?;
        self.args(args)
    }

    fn read(&self, out: &mut [u16]) -> Result<()> {
        let device = self.read.as_ref().unwrap_or(&self.write);
        let mut bytes = [0u8; (DEV_INFO_WORDS + 1) * 2];
        let bytes = &mut bytes[..(out.len() + 1) * 2];
        self.wait_ready(HRDY_TIMEOUT)?;
        Self::selected(self.cs, device, || {
            word(device, PRE_READ)?;
            self.wait_ready(HRDY_TIMEOUT)?;
            device.receive_polling(bytes)
        })?;
        // The first word after the preamble is a dummy.
        for (out, pair) in out.iter_mut().zip(bytes[2..].chunks_exact(2)) {
            *out = u16::from_be_bytes([pair[0], pair[1]]);
        }
        Ok(())
    }

    fn read_reg(&self, reg: u16) -> Result<u16> {
        self.command_args(CMD_REG_RD, &[reg])?;
        let mut value = [0];
        self.read(&mut value)?;
        Ok(value[0])
    }

    fn write_reg(&self, reg: u16, value: u16) -> Result<()> {
        self.command_args(CMD_REG_WR, &[reg, value])
    }

    /// Sends `rows` back to back after one preamble, copied through the bounce
    /// buffers so they may sit where DMA cannot read.
    fn load<'a>(&mut self, arg0: u16, area: Rect, rows: impl Iterator<Item = &'a [u8]>) -> Result<()> {
        self.command_args(CMD_LD_IMG_AREA, &[arg0, area.x as u16, area.y as u16, area.w as u16, area.h as u16])?;
        self.wait_ready(HRDY_TIMEOUT)?;
        let Self { write, cs, hrdy, bounce, transfers, .. } = self;
        Self::selected(*cs, write, || {
            word(write, PRE_WRITE)?;
            wait_ready(*hrdy, HRDY_TIMEOUT)?;
            stream(write, bounce, transfers, rows)
        })?;
        self.command(CMD_LD_IMG_END)
    }

    fn drives_done(&self) -> Result<bool> {
        Ok(self.read_reg(REG_LUTAFSR)? == 0)
    }

    fn wait_drives(&self) -> Result<()> {
        self.wait_ready(DRIVE_TIMEOUT)?;
        let start = Instant::now();
        while !self.drives_done()? {
            if start.elapsed() > DRIVE_TIMEOUT {
                error!(TAG, "drive still running after {DRIVE_TIMEOUT:?}");
                return Err(Error::TIMEOUT);
            }
            thread::sleep(BUSY_POLL);
        }
        Ok(())
    }
}

fn stream<'a>(device: &Device, bounce: &mut [Box<[u8], Caps>; 2], transfers: &mut [Transfer; 2], rows: impl Iterator<Item = &'a [u8]>) -> Result<()> {
    let mut queued = 0;
    let result = (|| {
        let (mut turn, mut fill) = (0, 0);
        for mut row in rows {
            while !row.is_empty() {
                if fill == 0 && queued == 2 {
                    device.finish()?;
                    queued -= 1;
                }
                let n = row.len().min(BOUNCE_BYTES - fill);
                bounce[turn][fill..fill + n].copy_from_slice(&row[..n]);
                (fill, row) = (fill + n, &row[n..]);
                if fill == BOUNCE_BYTES {
                    unsafe { device.queue_transmit(&mut transfers[turn], &bounce[turn][..fill]) }?;
                    (queued, turn, fill) = (queued + 1, 1 - turn, 0);
                }
            }
        }
        if fill > 0 {
            if queued == 2 {
                device.finish()?;
                queued -= 1;
            }
            unsafe { device.queue_transmit(&mut transfers[turn], &bounce[turn][..fill]) }?;
            queued += 1;
        }
        Ok(())
    })();
    while queued > 0 {
        device.finish()?;
        queued -= 1;
    }
    result
}

struct State {
    io: Io,
    address: u32,
    running: bool,
    last: Instant,
    /// Unturned areas driven since the drives were last seen done.
    driving: Option<Rect>,
}

impl State {
    fn wake(&mut self) -> Result<()> {
        if !self.running {
            self.io.command(CMD_SYS_RUN)?;
            self.running = true;
        }
        self.last = Instant::now();
        Ok(())
    }

    fn settle(&mut self, area: Rect) -> Result<()> {
        if self.driving.is_some_and(|d| d.intersects(&area)) {
            self.io.wait_drives()?;
            self.driving = None;
        }
        Ok(())
    }

    fn drive(&mut self, area: Rect, mode: u16, panel: Size) -> Result<()> {
        let area = widen(area, panel);
        self.settle(area)?;
        let address = [self.address as u16, (self.address >> 16) as u16];
        self.io.command_args(CMD_DPY_BUF_AREA, &[area.x as u16, area.y as u16, area.w as u16, area.h as u16, mode, address[0], address[1]])?;
        self.driving = Some(self.driving.map_or(area, |d| d.union(&area)));
        Ok(())
    }
}

/// Stands the chip by once nothing has been sent for a while.
struct Rest {
    state: Arc<Mutex<State>>,
    after: Duration,
}

impl Source for Rest {
    fn tick(&mut self) -> Next {
        let mut state = self.state.lock();
        if !state.running {
            return Next::Idle;
        }
        let idle = state.last.elapsed();
        if idle < self.after {
            return Next::After(self.after - idle);
        }
        match state.io.drives_done() {
            Ok(true) => {}
            Ok(false) => return Next::After(BUSY_POLL),
            Err(err) => {
                error!(TAG, "rest: {err}");
                return Next::Idle;
            }
        }
        if let Err(err) = state.io.command(CMD_STANDBY) {
            error!(TAG, "standby: {err}");
            return Next::Idle;
        }
        state.running = false;
        state.driving = None;
        Next::Idle
    }
}

fn quarters(rotation: Rotation) -> u16 {
    match rotation {
        Rotation::R0 => 0,
        Rotation::R90 => 1,
        Rotation::R180 => 2,
        Rotation::R270 => 3,
    }
}

/// Where `area` of the picture turned by `rotation` lies on the `panel`.
fn unturn(area: Rect, rotation: Rotation, panel: Size) -> Rect {
    let (pw, ph) = (panel.width as i16, panel.height as i16);
    match rotation {
        Rotation::R0 => area,
        Rotation::R90 => Rect::new(area.y, ph - area.w - area.x, area.h, area.w),
        Rotation::R180 => Rect::new(pw - area.w - area.x, ph - area.h - area.y, area.w, area.h),
        Rotation::R270 => Rect::new(pw - area.h - area.y, area.x, area.h, area.w),
    }
}

// A drive whose columns all fall in one group of four can freeze the chip.
fn widen(area: Rect, panel: Size) -> Rect {
    let (left, right) = (area.x & !3, (area.right() - 1) & !3);
    if left != right {
        return area;
    }
    if left + 8 <= panel.width as i16 {
        Rect::new(area.x, area.y, left + 8 - area.x, area.h)
    } else {
        Rect::new(left - 4, area.y, area.right() - (left - 4), area.h)
    }
}

fn bpp(format: ImageFormat) -> Option<u16> {
    match format {
        ImageFormat::L4 => Some(BPP_4),
        ImageFormat::L8 => Some(BPP_8),
        _ => None,
    }
}

fn mode_of(waveform: Waveform, all: bool) -> u16 {
    match (waveform, all) {
        (Waveform::Fast, _) => MODE_DU,
        (Waveform::Text, false) => MODE_GL16,
        (Waveform::Quality, _) | (Waveform::Text, true) => MODE_GC16,
    }
}

const fn encode(mode: EpdMode) -> u8 {
    match mode {
        EpdMode::Hold => 0,
        EpdMode::Seed => 1,
        EpdMode::Refresh(Waveform::Fast) => 2,
        EpdMode::Refresh(Waveform::Quality) => 3,
        EpdMode::Refresh(Waveform::Text) => 4,
    }
}

const fn decode(mode: u8) -> EpdMode {
    match mode {
        1 => EpdMode::Seed,
        2 => EpdMode::Refresh(Waveform::Fast),
        3 => EpdMode::Refresh(Waveform::Quality),
        4 => EpdMode::Refresh(Waveform::Text),
        _ => EpdMode::Hold,
    }
}

pub(crate) struct It8951e {
    state: Arc<Mutex<State>>,
    rest: Notifier,
    size: Size,
    ppi: u16,
    format: ImageFormat,
    rotation: Rotation,
    mode: AtomicU8,
    issued: AtomicU32,
}

impl It8951e {
    /// The chip must be powered and booted far enough to drive HRDY.
    pub(crate) fn new(config: Config) -> Result<Self> {
        if bpp(config.pixel_format).is_none() {
            error!(TAG, "pixel format {:?}: only L4 and L8", config.pixel_format);
            return Err(Error::NOT_SUPPORTED);
        }
        gpio::configure(&[config.hrdy], gpio::Mode::Input)?;
        gpio::set_pull(config.hrdy, gpio::Pull::Up)?;
        gpio::configure(&[config.cs], gpio::Mode::Output)?;
        gpio::set_level(config.cs, true)?;
        let io = Io::new(&config.bus, config.cs, config.hrdy, config.write_hz, config.read_hz, config.input_delay_ns)
            .inspect_err(|err| error!(TAG, "spi: {err}"))?;
        let address = start(&io, &config).inspect_err(|err| error!(TAG, "start: {err}"))?;
        let state = Arc::new(Mutex::new(State { io, address, running: true, last: Instant::now(), driving: None }));
        let rest = dispatch::add(Box::new(Rest { state: state.clone(), after: config.rest_after }))?;
        Ok(Self {
            state,
            rest,
            size: config.size,
            ppi: config.ppi,
            format: config.pixel_format,
            rotation: config.rotation,
            mode: AtomicU8::new(encode(EpdMode::Hold)),
            issued: AtomicU32::new(0),
        })
    }

    /// Runs `f` with the chip awake, and lets it rest later.
    fn with<R>(&self, f: impl FnOnce(&mut State) -> Result<R>) -> Result<R> {
        let result = {
            let mut state = self.state.lock();
            state.wake().and_then(|()| f(&mut state))
        };
        self.rest.notify();
        result
    }

    fn put<'a>(&self, area: Rect, rows: impl Iterator<Item = &'a [u8]>) -> Result<()> {
        let panel = self.size;
        let placed = unturn(area, self.rotation, panel);
        let arg0 = BIG_ENDIAN << 8 | bpp(self.format).unwrap_or(BPP_8) << 4 | quarters(self.rotation);
        let mode = decode(self.mode.load(Ordering::Relaxed));
        self.with(|state| {
            state.settle(placed)?;
            state.io.load(arg0, area, rows)?;
            // The chip tracks the glass itself, so a seed cannot tell it what
            // is already there and only loads.
            match mode {
                EpdMode::Hold | EpdMode::Seed => Ok(()),
                EpdMode::Refresh(waveform) => state.drive(placed, mode_of(waveform, false), panel),
            }
        })
    }
}

/// Returns the image buffer's address.
fn start(io: &Io, config: &Config) -> Result<u32> {
    io.wait_ready(HRDY_TIMEOUT)?;
    io.command(CMD_GET_DEV_INFO)?;
    let mut dev_info = [0u16; DEV_INFO_WORDS];
    io.read(&mut dev_info)?;
    let size = Size::new(dev_info[0], dev_info[1]);
    let address = u32::from(dev_info[3]) << 16 | u32::from(dev_info[2]);
    let address = if size == config.size {
        info!(TAG, "{}x{}, image buffer at 0x{address:08x}", size.width, size.height);
        address
    } else {
        warn!(TAG, "device info reads {}x{}, not {}x{}; image buffer assumed at 0x{:08x}",
            size.width, size.height, config.size.width, config.size.height, config.fallback_address);
        config.fallback_address
    };
    io.command(CMD_SYS_RUN)?;
    io.write_reg(REG_I80CPCR, PACKED_WRITE)?;
    io.write_reg(REG_LISAR + 2, (address >> 16) as u16)?;
    io.write_reg(REG_LISAR, address as u16)?;
    io.command_args(CMD_VCOM, &[VCOM_GET])?;
    let mut vcom = [0];
    io.read(&mut vcom)?;
    if vcom[0] != config.vcom_mv {
        io.command_args(CMD_VCOM, &[VCOM_SET, config.vcom_mv])?;
    }
    Ok(address)
}

impl Driver for It8951e {
    type FrameBuffers = Absent;
    type Gram = Self;
    type Epd = Self;

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
        Rotations::all()
    }

    // Each 16-bit word the chip takes holds whole pixels, along rows of the
    // turned picture and so along either axis of the panel.
    fn alignment(&self) -> u16 {
        if self.format == ImageFormat::L4 { 4 } else { 2 }
    }

    fn gram(&self) -> Option<&Self> {
        Some(self)
    }

    fn epd(&self) -> Option<&Self> {
        Some(self)
    }

    fn draw(&self, area: Rect, image: &Image) -> Result<()> {
        let row = self.format.row_bytes(area.w as usize);
        let stride = image.stride_bytes();
        self.put(area, (0..area.h as usize).map(|j| &image.pixels()[j * stride..][..row]))
    }

    fn reconfigure(&mut self, format: ImageFormat, rotation: Rotation) -> Result<()> {
        if bpp(format).is_none() {
            return Err(Error::NOT_SUPPORTED);
        }
        self.format = format;
        self.rotation = rotation;
        Ok(())
    }
}

impl GramDriver for It8951e {
    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        self.put(area, iter::once(pixels))?;
        Ok(Ticket(self.issued.fetch_add(1, Ordering::Relaxed).wrapping_add(1)))
    }

    fn wait(&self, _ticket: Ticket) -> Result<()> {
        Ok(())
    }
}

impl EpdDriver for It8951e {
    fn set_mode(&self, mode: EpdMode) -> Result<()> {
        self.mode.store(encode(mode), Ordering::Relaxed);
        Ok(())
    }

    fn refresh(&self, area: Rect, waveform: Waveform, all: bool) -> Result<()> {
        let panel = self.size;
        let placed = unturn(area, self.rotation, panel);
        self.with(|state| state.drive(placed, mode_of(waveform, all), panel))
    }

    fn clear(&self) -> Result<()> {
        let panel = self.size;
        let whole = Rect::new(0, 0, panel.width as i16, panel.height as i16);
        let bytes = usize::from(panel.width) * usize::from(panel.height) / 2;
        let rows = iter::repeat_n(&WHITE[..], bytes / WHITE.len()).chain(iter::once(&WHITE[..bytes % WHITE.len()]));
        self.with(|state| {
            state.settle(whole)?;
            state.io.load(BIG_ENDIAN << 8 | BPP_4 << 4, whole, rows)?;
            state.drive(whole, MODE_INIT, panel)
        })
    }

    fn wait_idle(&self) -> Result<()> {
        let mut state = self.state.lock();
        if state.running && state.driving.is_some() {
            state.io.wait_drives()?;
            state.driving = None;
        }
        Ok(())
    }
}
