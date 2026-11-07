/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;

use devkit_gfx::{ImageFormat, Rect, Rotation, Target};
use idf::{Error, Result};

use crate::ppa::Yuv;
use crate::wait::{self, Pollable};

mod header;

#[cfg(target_os = "none")]
#[path = "hw.rs"]
mod backend;
#[cfg(not(target_os = "none"))]
#[path = "sim.rs"]
mod backend;

pub use header::{Header, Sampling};

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(200);
/// The 2D engine works in 16-row blocks, so strips end on them.
const STRIP_ROW_ALIGN: u16 = 16;
const MAX_STRIP_BUFFERS: usize = 2;
const BUFFER_ALIGN: usize = 64;

/// What the decoder writes: `Rgb888`, `Bgr888` or `Rgb565` from a colour
/// picture, `L8` from a gray one.
#[derive(Clone, Copy)]
pub struct DecodeConfig {
    pub format: ImageFormat,
    pub yuv: Yuv,
}

pub struct Decoder {
    config: DecodeConfig,
    timeout: Duration,
}

#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub buffers: [*mut u8; MAX_STRIP_BUFFERS],
    pub count: usize,
    pub rows: u16,
    pub strip_count: u16,
}

#[derive(Clone, Copy)]
pub(crate) enum Output {
    Whole(*mut u8, #[cfg_attr(not(target_os = "none"), allow(dead_code))] usize),
    Strips(Layout),
}

pub(crate) struct Frame<'a> {
    pub header: Header,
    pub config: DecodeConfig,
    pub data: &'a [u8],
    pub output: Output,
    pub row_bytes: usize,
    #[cfg_attr(not(target_os = "none"), allow(dead_code))]
    pub timeout: Duration,
}

impl Decoder {
    pub fn new(config: &DecodeConfig) -> Result<Self> {
        if !matches!(config.format, ImageFormat::Rgb888 | ImageFormat::Bgr888 | ImageFormat::Rgb565 | ImageFormat::L8) {
            return Err(Error::NOT_SUPPORTED);
        }
        backend::init()?;
        Ok(Self { config: *config, timeout: DEFAULT_TIMEOUT })
    }

    /// How long the decoder may go without writing anything; 200 ms at first.
    /// Time a caller holds a strip does not count.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    fn frame<'a>(&self, data: &'a [u8], output: impl FnOnce(&Header, usize) -> Result<Output>) -> Result<Frame<'a>> {
        let header = Header::parse(data)?;
        let gray = header.sampling == Sampling::Gray;
        if gray != (self.config.format == ImageFormat::L8) {
            return Err(Error::NOT_SUPPORTED);
        }
        let row_bytes = self.config.format.row_bytes(usize::from(header.padded_width));
        let output = output(&header, row_bytes)?;
        Ok(Frame { header, config: self.config, data, output, row_bytes, timeout: self.timeout })
    }

    /// The whole picture into `target`, from its top left: not turned, in the
    /// configured format, `padded_width` wide in stride and at least
    /// `padded_height` tall.
    pub fn decode<'a>(&'a mut self, data: &'a [u8], target: &'a mut Target<'_>) -> Result<Decode<'a>> {
        let frame = self.frame(data, |h, row_bytes| {
            let fits = target.rotation() == Rotation::R0
                && target.format() == self.config.format
                && target.stride() == usize::from(h.padded_width)
                && target.size().height >= h.padded_height
                && target.area() == Rect::new(0, 0, target.size().width as i16, target.size().height as i16);
            if !fits {
                return Err(Error::INVALID_ARG);
            }
            Ok(Output::Whole(target.as_ptr(), row_bytes * usize::from(h.padded_height)))
        })?;
        Ok(Decode { session: backend::Session::new(frame)?, _borrow: PhantomData })
    }

    /// The picture a band of rows at a time, through one or two buffers of
    /// `BUFFER_ALIGN`-aligned memory the 2D-DMA reaches. Rows are
    /// `padded_width` wide; strips are a multiple of 16 rows tall.
    pub fn strips<'a, 'b: 'a>(&'a mut self, data: &'a [u8], buffers: &'a mut [&'b mut [u8]]) -> Result<Strips<'a>> {
        if buffers.is_empty() || buffers.len() > MAX_STRIP_BUFFERS {
            return Err(Error::INVALID_ARG);
        }
        let usable = buffers.iter().map(|b| b.len()).min().unwrap_or(0);
        if buffers.iter().any(|b| b.as_ptr() as usize % BUFFER_ALIGN != 0) {
            return Err(Error::INVALID_ARG);
        }
        let count = buffers.len();
        let mut ptrs = [core::ptr::null_mut(); MAX_STRIP_BUFFERS];
        for (p, b) in ptrs.iter_mut().zip(buffers.iter_mut()) {
            *p = b.as_mut_ptr();
        }
        let frame = self.frame(data, |h, row_bytes| {
            let fit = (usable / row_bytes) as u16;
            let mut rows = fit - fit % STRIP_ROW_ALIGN;
            let share = h.padded_height.div_ceil(count as u16).next_multiple_of(STRIP_ROW_ALIGN);
            rows = rows.min(share);
            if rows == 0 {
                return Err(Error::INVALID_SIZE);
            }
            let strip_count = h.padded_height.div_ceil(rows);
            Ok(Output::Strips(Layout { buffers: ptrs, count, rows, strip_count }))
        })?;
        let header = frame.header;
        let Output::Strips(layout) = frame.output else { return Err(Error::INVALID_STATE) };
        Ok(Strips { session: backend::Session::new(frame)?, header, layout, _borrow: PhantomData })
    }
}

/// A picture being decoded. Await it or `wait` for it; dropping it stops the
/// decoder. Like a PPA job it must not be leaked while it runs.
pub struct Decode<'a> {
    session: backend::Session,
    _borrow: PhantomData<&'a mut ()>,
}

impl Decode<'_> {
    pub fn wait(mut self) -> Result<()> {
        self.session.blocking = true;
        loop {
            let slice = self.session.slice();
            if let Some(result) = wait::block(&mut self.session, Some(slice)) {
                return result.map(|_| ());
            }
        }
    }
}

impl Future for Decode<'_> {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let this = self.get_mut();
        this.session.blocking = false;
        this.session.poll(cx).map(|r| r.map(|_| ()))
    }
}

/// A picture handed out a strip at a time, in order. Holding a strip keeps its
/// buffer from the decoder, which goes on with the other one if there is.
pub struct Strips<'a> {
    session: backend::Session,
    header: Header,
    layout: Layout,
    _borrow: PhantomData<&'a mut ()>,
}

impl Strips<'_> {
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Rows of every strip but maybe the last.
    pub fn strip_rows(&self) -> u16 {
        self.layout.rows
    }

    pub async fn next(&mut self) -> Result<Option<Strip<'_>>> {
        self.session.blocking = false;
        let index = core::future::poll_fn(|cx| self.session.poll(cx)).await?;
        Ok(index.map(|i| self.strip(i)))
    }

    /// From a task that is not async.
    pub fn next_blocking(&mut self) -> Result<Option<Strip<'_>>> {
        self.session.blocking = true;
        let index = loop {
            let slice = self.session.slice();
            if let Some(result) = wait::block(&mut self.session, Some(slice)) {
                break result?;
            }
        };
        Ok(index.map(|i| self.strip(i)))
    }

    fn strip(&mut self, index: u16) -> Strip<'_> {
        let l = &self.layout;
        let y = index * l.rows;
        let padded = l.rows.min(self.header.padded_height - y);
        let rows = padded.min(self.header.height.saturating_sub(y));
        let row_bytes = self.session.row_bytes();
        let ptr = l.buffers[usize::from(index) % l.count];
        let pixels = unsafe { core::slice::from_raw_parts(ptr.cast_const(), row_bytes * usize::from(padded)) };
        Strip { session: &mut self.session, index, y, rows, padded, pixels }
    }
}

pub struct Strip<'s> {
    session: &'s mut backend::Session,
    index: u16,
    y: u16,
    rows: u16,
    padded: u16,
    pixels: &'s [u8],
}

impl Strip<'_> {
    pub fn index(&self) -> u16 {
        self.index
    }

    /// The first row's place in the picture.
    pub fn y(&self) -> u16 {
        self.y
    }

    /// Rows of the picture in it, without the MCU padding below the last one.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Rows the decoder wrote.
    pub fn padded_rows(&self) -> u16 {
        self.padded
    }

    /// `padded_rows` rows of `stride_bytes` each.
    pub fn pixels(&self) -> &[u8] {
        self.pixels
    }

    pub fn stride_bytes(&self) -> usize {
        self.session.row_bytes()
    }
}

impl Drop for Strip<'_> {
    fn drop(&mut self) {
        self.session.release(self.index);
    }
}

#[cfg(test)]
mod tests;
