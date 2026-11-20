/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::CStr;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use idf::log::error;
use idf::sync::{Mutex, Receiver, Sender, Semaphore, channel};
use idf::{Error, Result, thread};

use super::Host;
use super::protocol::{Event, PixelFormat};
use crate::display::{Driver, Image, ImageFormat, Rect, Rotation, Size, Ticket};

const TAG: &CStr = c"sim_gram";
const QUEUE_DEPTH: usize = 10;
const MAX_WAITERS: u32 = 8;
const STALL_TIMEOUT: Duration = Duration::from_secs(2);

enum Command {
    Write { ticket: u32, area: Rect, pixels: *const u8, len: usize },
    Rotate(Rotation),
}

// The writer keeps the pixels until `wait`, as for a DMA transfer.
unsafe impl Send for Command {}

struct Progress {
    done: AtomicU32,
    waiters: AtomicU32,
    wake: Semaphore,
}

impl Progress {
    fn reached(&self, ticket: u32) -> bool {
        self.done.load(Ordering::SeqCst).wrapping_sub(ticket) as i32 >= 0
    }
}

/// A panel that keeps the picture in its own memory, on the host. Pixels are
/// read when they go out, and the host answers once the panel's link would
/// have carried them.
pub(crate) struct SimGram {
    host: &'static Host,
    size: Size,
    ppi: u16,
    format: ImageFormat,
    alignment: u16,
    quarter_turns: bool,
    rotation: Rotation,
    issued: Mutex<u32>,
    queue: Sender<Command>,
    progress: Arc<Progress>,
}

fn quarters(rotation: Rotation) -> u8 {
    match rotation {
        Rotation::R0 => 0,
        Rotation::R90 => 1,
        Rotation::R180 => 2,
        Rotation::R270 => 3,
    }
}

fn send(host: &Host, queue: Receiver<Command>) {
    loop {
        match queue.recv() {
            Command::Write { ticket, area, pixels, len } => {
                let payload = unsafe { core::slice::from_raw_parts(pixels, len) };
                let event = Event::GramWrite {
                    ticket,
                    x: area.x as u16,
                    y: area.y as u16,
                    w: area.w as u16,
                    h: area.h as u16,
                    bytes: len as u32,
                };
                host.send_with(&event, payload);
            }
            Command::Rotate(rotation) => host.send(&Event::GramRotate { rotation: quarters(rotation) }),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GramConfig {
    /// Unturned.
    pub size: Size,
    pub ppi: u16,
    pub format: ImageFormat,
    pub alignment: u16,
    /// Whether it turns by quarters as well as halves.
    pub quarter_turns: bool,
    pub bytes_per_second: u32,
}

impl SimGram {
    pub(crate) fn new(host: &'static Host, config: &GramConfig, rotation: Rotation) -> Result<Self> {
        let GramConfig { size, ppi, format, alignment, quarter_turns, bytes_per_second } = *config;
        if rotation.swaps_axes() && !quarter_turns {
            return Err(Error::NOT_SUPPORTED);
        }
        let wire = match format {
            ImageFormat::Rgb565 => PixelFormat::Rgb565,
            ImageFormat::Rgb565Be => PixelFormat::Rgb565Be,
            ImageFormat::Bgr888 => PixelFormat::Bgr888,
            _ => return Err(Error::NOT_SUPPORTED),
        };
        let progress = Arc::new(Progress { done: AtomicU32::new(0), waiters: AtomicU32::new(0), wake: Semaphore::try_new(0, MAX_WAITERS)? });
        let listener = progress.clone();
        host.set_gram_listener(Box::new(move |ticket| {
            listener.done.store(ticket, Ordering::SeqCst);
            for _ in 0..listener.waiters.load(Ordering::SeqCst) {
                listener.wake.give();
            }
        }));
        host.send(&Event::GramOpen { width: size.width, height: size.height, format: wire, rotation: quarters(rotation), bytes_per_second });
        let (queue, commands) = channel(QUEUE_DEPTH);
        thread::Builder::new().name("sim_gram").stack_size(4096).spawn(move || send(host, commands))?;
        Ok(Self { host, size, ppi, format, alignment, quarter_turns, rotation, issued: Mutex::new(0), queue, progress })
    }
}

impl Driver for SimGram {
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

    fn alignment(&self) -> u16 {
        self.alignment
    }

    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        let mut issued = self.issued.lock();
        *issued = issued.wrapping_add(1);
        self.queue.send(Command::Write { ticket: *issued, area, pixels: pixels.as_ptr(), len: pixels.len() });
        Ok(Ticket(*issued))
    }

    fn wait(&self, ticket: Ticket) -> Result<()> {
        let progress = &*self.progress;
        while !progress.reached(ticket.0) {
            progress.waiters.fetch_add(1, Ordering::SeqCst);
            if !progress.reached(ticket.0) && !progress.wake.take_timeout(STALL_TIMEOUT) {
                error!(TAG, "ticket {} not answered, at {}", ticket.0, progress.done.load(Ordering::SeqCst));
            }
            progress.waiters.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }

    fn draw(&self, area: Rect, image: &Image) -> Result<()> {
        let row = self.format.row_bytes(area.w as usize);
        let mut last = None;
        for j in 0..area.h {
            let pixels = &image.pixels()[j as usize * image.stride_bytes()..][..row];
            last = Some(unsafe { self.write(Rect::new(area.x, area.y + j, area.w, 1), pixels) }?);
        }
        last.map_or(Ok(()), |ticket| self.wait(ticket))
    }

    fn set_brightness(&self, level: u8) -> Result<()> {
        self.host.send(&Event::Brightness { level: level.min(100) });
        Ok(())
    }

    fn reconfigure(&mut self, format: ImageFormat, fb_num: u8, rotation: Rotation) -> Result<()> {
        if format != self.format || fb_num != 0 || (rotation.swaps_axes() && !self.quarter_turns) {
            return Err(Error::NOT_SUPPORTED);
        }
        self.queue.send(Command::Rotate(rotation));
        self.rotation = rotation;
        Ok(())
    }
}
