/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::future::Future;
use core::marker::PhantomData;
use core::ops::Deref;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;

use core::ptr::NonNull;

use devkit_gfx::{Image, ImageFormat, Rect, Rotation, Size, Target};
use idf::{Error, Result};

use crate::ppa::{Alpha, Placement, Plan, Ppa, Source, Yuv, srm_plan};
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

/// How `render` lays the picture into its target, as an `Srm` lays its source:
/// `src_rect` of the picture, `None` for all of it, scaled, turned and mirrored
/// with its top left at `at`, cut off at the target's area.
#[derive(Clone, Copy)]
pub struct Transform {
    pub src_rect: Option<Rect>,
    pub at: (i16, i16),
    pub rotation: Rotation,
    pub scale_x: f32,
    pub scale_y: f32,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub dst_yuv: Yuv,
}

impl Transform {
    pub fn new() -> Self {
        Self {
            src_rect: None,
            at: (0, 0),
            rotation: Rotation::R0,
            scale_x: 1.0,
            scale_y: 1.0,
            mirror_x: false,
            mirror_y: false,
            dst_yuv: Yuv::default(),
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::new()
    }
}

/// What turns each strip into an engine job: the cut placement of the whole
/// picture and the target, kept raw so an interrupt can plan with them.
#[derive(Clone, Copy)]
pub(crate) struct Chain {
    pub ppa: &'static Ppa,
    placement: Placement,
    dst_yuv: Yuv,
    format: ImageFormat,
    padded_width: u16,
    target: (NonNull<u8>, usize, ImageFormat, Size, usize, Rotation, Rect),
}

unsafe impl Send for Chain {}

impl Chain {
    fn target(&self) -> Option<Target<'static>> {
        let (ptr, len, format, size, stride, rotation, area) = self.target;
        unsafe { Target::from_raw(ptr, len, format, size, stride) }?.rotated(rotation).crop(area)
    }

    /// The job for the strip whose `padded` rows from picture row `y` are at
    /// `buffer`; `None` when nothing of it lands.
    pub fn plan(&self, buffer: *const u8, y: u16, padded: u16) -> Result<Option<Plan>> {
        let src = self.placement.src_rect;
        let (top, bottom) = ((y as i16).max(src.y), (y as i16 + padded as i16).min(src.bottom()));
        if bottom <= top {
            return Ok(None);
        }
        let part = self.placement.part(Rect::new(src.x, top, src.w, bottom - top));
        let len = self.format.row_bytes(usize::from(self.padded_width)) * usize::from(padded);
        let pixels = unsafe { core::slice::from_raw_parts(buffer, len) };
        let image = Image::from_pixels(self.format, self.padded_width, padded, usize::from(self.padded_width), pixels);
        let source = Source { image, yuv: Yuv::default(), rgb_swap: false, byte_swap: false, alpha: Alpha::Keep };
        let placed = Placement { src_rect: part.src_rect.offset(0, -(y as i16)), ..part };
        let mut target = self.target().ok_or(Error::INVALID_ARG)?;
        srm_plan(&source, placed, self.dst_yuv, &mut target)
    }
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

/// The header through the start of its scan, in one piece.
pub(crate) enum Head<'a> {
    Lent(&'a [u8]),
    Joined(Vec<u8>),
}

impl Deref for Head<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Head::Lent(head) => head,
            Head::Joined(head) => head,
        }
    }
}

pub(crate) struct Frame<'a> {
    pub header: Header,
    pub config: DecodeConfig,
    #[cfg_attr(not(target_os = "none"), allow(dead_code))]
    pub head: Head<'a>,
    pub data: &'a [&'a [u8]],
    pub output: Output,
    pub chain: Option<Chain>,
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

    fn frame<'a>(&self, data: &'a [&'a [u8]], output: impl FnOnce(&Header, usize) -> Result<Output>) -> Result<Frame<'a>> {
        let (header, head) = head(data)?;
        let gray = header.sampling == Sampling::Gray;
        if gray != (self.config.format == ImageFormat::L8) {
            return Err(Error::NOT_SUPPORTED);
        }
        let row_bytes = self.config.format.row_bytes(usize::from(header.padded_width));
        let output = output(&header, row_bytes)?;
        Ok(Frame { header, config: self.config, head, data, output, chain: None, row_bytes, timeout: self.timeout })
    }

    /// The whole picture into `target`, from its top left: not turned, in the
    /// configured format, `padded_width` wide in stride and at least
    /// `padded_height` tall. `data` is the file in as many pieces as it is
    /// held in.
    pub fn decode<'a>(&'a mut self, data: &'a [&'a [u8]], target: &'a mut Target<'_>) -> Result<Decode<'a>> {
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
    pub fn strips<'a, 'b: 'a>(&'a mut self, data: &'a [&'a [u8]], buffers: &'a mut [&'b mut [u8]]) -> Result<Strips<'a>> {
        let frame = self.strip_frame(data, buffers)?;
        let header = frame.header;
        let Output::Strips(layout) = frame.output else { return Err(Error::INVALID_STATE) };
        Ok(Strips { session: backend::Session::new(frame)?, header, layout, _borrow: PhantomData })
    }

    /// The picture through strips, as `strips`, each laid into `target` by the
    /// 2D engine as soon as it is decoded: `transform` says where and how.
    /// Every strip boundary inside the picture's part must land on a whole
    /// row, or it is `INVALID_ARG`.
    pub fn render<'a, 'b: 'a>(
        &'a mut self,
        data: &'a [&'a [u8]],
        buffers: &'a mut [&'b mut [u8]],
        transform: &Transform,
        target: &'a mut Target<'_>,
    ) -> Result<Render<'a>> {
        let ppa = crate::ppa::ppa()?;
        let mut frame = self.strip_frame(data, buffers)?;
        let h = frame.header;
        let Output::Strips(layout) = frame.output else { return Err(Error::INVALID_STATE) };
        let picture = Rect::new(0, 0, h.width as i16, h.height as i16);
        let src = transform.src_rect.unwrap_or(picture);
        if src.is_empty() || !picture.contains(&src) {
            return Err(Error::INVALID_ARG);
        }
        let t = transform;
        let whole = Placement::new(src, t.at, t.scale_x, t.scale_y, t.rotation, t.mirror_x, t.mirror_y)?;
        let step = whole.row_step() as i16;
        for k in 1..layout.strip_count {
            let edge = (k * layout.rows) as i16;
            if edge > src.y && edge < src.bottom() && (edge - src.y) % step != 0 {
                return Err(Error::INVALID_ARG);
            }
        }
        let Some(placement) = whole.clip(target.area(), step as u32, (false, false)) else {
            return Ok(Render { session: None, _borrow: PhantomData });
        };
        let raw = NonNull::new(target.as_ptr()).ok_or(Error::INVALID_ARG)?;
        let chain = Chain {
            ppa,
            placement,
            dst_yuv: t.dst_yuv,
            format: self.config.format,
            padded_width: h.padded_width,
            target: (raw, target.len(), target.format(), target.size(), target.stride(), target.rotation(), target.area()),
        };
        for i in 0..layout.strip_count {
            let y = i * layout.rows;
            let padded = layout.rows.min(h.padded_height - y);
            if let Some(plan) = chain.plan(layout.buffers[usize::from(i) % layout.count], y, padded)? {
                ppa.check(&plan)?;
            }
        }
        ppa.sync_target(target, placement.landed())?;
        frame.chain = Some(chain);
        Ok(Render { session: Some(backend::Session::new(frame)?), _borrow: PhantomData })
    }

    fn strip_frame<'a>(&self, data: &'a [&'a [u8]], buffers: &mut [&mut [u8]]) -> Result<Frame<'a>> {
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
        self.frame(data, |h, row_bytes| {
            let fit = (usable / row_bytes) as u16;
            let mut rows = fit - fit % STRIP_ROW_ALIGN;
            let share = h.padded_height.div_ceil(count as u16).next_multiple_of(STRIP_ROW_ALIGN);
            rows = rows.min(share);
            if rows == 0 {
                return Err(Error::INVALID_SIZE);
            }
            let strip_count = h.padded_height.div_ceil(rows);
            Ok(Output::Strips(Layout { buffers: ptrs, count, rows, strip_count }))
        })
    }
}

/// The header out of the pieces, joined only when the first ends inside it.
fn head<'a>(data: &'a [&'a [u8]]) -> Result<(Header, Head<'a>)> {
    let first = data.first().copied().unwrap_or_default();
    match header::parse(first, None) {
        Ok(header) => return Ok((header, Head::Lent(first))),
        Err(err) if data.len() < 2 => return Err(err),
        Err(_) => {}
    }
    let mut joined = Vec::new();
    for part in data {
        joined.try_reserve(part.len()).map_err(|_| Error::NO_MEM)?;
        joined.extend_from_slice(part);
        if let Ok(header) = header::parse(&joined, None) {
            return Ok((header, Head::Joined(joined)));
        }
    }
    Err(Error::INVALID_ARG)
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

/// A picture being laid into a target. Await it or `wait` for it; dropping it
/// stops the decoder and waits for the engine. It must not be leaked while it
/// runs.
pub struct Render<'a> {
    session: Option<backend::Session>,
    _borrow: PhantomData<&'a mut ()>,
}

impl Render<'_> {
    pub fn wait(mut self) -> Result<()> {
        let Some(session) = self.session.as_mut() else { return Ok(()) };
        session.blocking = true;
        loop {
            let slice = session.slice();
            if let Some(result) = wait::block(session, Some(slice)) {
                return result.map(|_| ());
            }
        }
    }
}

impl Future for Render<'_> {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let Some(session) = self.get_mut().session.as_mut() else { return Poll::Ready(Ok(())) };
        session.blocking = false;
        session.poll(cx).map(|r| r.map(|_| ()))
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
