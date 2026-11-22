/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::slice;
use core::sync::atomic::{AtomicU32, Ordering};
use idf::{Error, Result};

use crate::Absent;

pub use devkit_gfx::{Image, ImageFormat, Rect, Rotation, Size};
use devkit_gfx::Target;

#[cfg(all(target_os = "none", CONFIG_BSP_RS_DRIVER_EPD_LL))]
pub mod epd_ll;

#[derive(Default)]
#[non_exhaustive]
pub struct Config {
    /// 0 selects the board default.
    pub fb_num: u8,
    /// `None` selects the board default.
    pub pixel_format: Option<ImageFormat>,
    /// `None` selects R0. Only one of `Display::rotations` is taken.
    pub rotation: Option<Rotation>,
}

devkit_core::bitflags! {
    /// Turns a panel shows by itself.
    pub struct Rotations: u8 {
        const R0 = 1 << 0;
        const R90 = 1 << 1;
        const R180 = 1 << 2;
        const R270 = 1 << 3;
    }
}

impl Rotations {
    pub const fn of(rotation: Rotation) -> Self {
        match rotation {
            Rotation::R0 => Self::R0,
            Rotation::R90 => Self::R90,
            Rotation::R180 => Self::R180,
            Rotation::R270 => Self::R270,
        }
    }

    pub const fn has(&self, rotation: Rotation) -> bool {
        self.contains(Self::of(rotation))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Waveform {
    /// Black and white without flashing, other grays roughly: the quickest,
    /// for highlights.
    Fast,
    /// All grays, flashing, the cleanest picture.
    Quality,
    /// All grays with less flashing than `Quality` and less ghosting than
    /// `Fast`, for changing screens.
    Text,
}

/// What a `draw` or `Gram::write` does to the glass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EpdMode {
    /// Only the panel's memory changes, until `Epd::refresh`.
    Hold,
    /// The pixels are taken as already on the glass, which is not driven.
    Seed,
    /// The written area is driven with the waveform.
    Refresh(Waveform),
    /// As `Refresh`, driving every pixel written, not only the changed ones.
    RefreshAll(Waveform),
}

/// What refreshes of separate areas can do together.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RefreshUnit {
    /// Every pixel drives on its own: any areas, at any time.
    Pixel,
    /// Up to this many rectangles drive at once; one more waits for those it
    /// overlaps.
    Rects(u8),
    /// The whole panel drives at once, whatever area is asked.
    Panel,
}

/// A transfer queued by `Gram::write`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ticket(pub(crate) u32);

#[derive(Clone, Copy)]
pub struct FrameBuffer {
    ptr: NonNull<u8>,
    size: Size,
    stride: usize,
    format: ImageFormat,
}

unsafe impl Send for FrameBuffer {}
unsafe impl Sync for FrameBuffer {}

impl FrameBuffer {
    #[cfg_attr(not(target_os = "none"), allow(dead_code))]
    pub(crate) unsafe fn new(ptr: NonNull<u8>, size: Size, format: ImageFormat) -> Self {
        let stride = format.row_bytes(usize::from(size.width));
        Self { ptr, size, stride, format }
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn format(&self) -> ImageFormat {
        self.format
    }

    pub fn len(&self) -> usize {
        self.stride * usize::from(self.size.height)
    }

    /// Nothing else may access the bytes written through the slice meanwhile.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn as_mut_slice(&self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.as_ptr(), self.len()) }
    }

    /// Nothing else may access the bytes written through the slice meanwhile.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn row_mut(&self, y: usize) -> &mut [u8] {
        assert!(y < usize::from(self.size.height));
        unsafe { slice::from_raw_parts_mut(self.as_ptr().add(y * self.stride), self.stride) }
    }

    /// The whole framebuffer as it is stored. Nothing else may write what the
    /// target, or one cropped from it, covers while it lives.
    pub unsafe fn target(&self) -> Option<Target<'static>> {
        let stride = usize::from(self.size.width);
        unsafe { Target::from_raw(self.ptr, self.len(), self.format, self.size, stride) }
    }
}

/// `None` when it is empty or starts left of or above the origin.
fn unsigned(rect: Rect) -> Option<(u32, u32, u32, u32)> {
    if rect.x < 0 || rect.y < 0 || rect.is_empty() {
        return None;
    }
    Some((rect.x as u32, rect.y as u32, rect.w as u32, rect.h as u32))
}

/// A capability the panel lacks is `Absent`, and its accessor stays `None`.
pub(crate) trait Driver: Send + Sync {
    type FrameBuffers: FrameBufferDriver;
    type Gram: GramDriver;
    type Epd: EpdDriver;

    /// After the rotation.
    fn size(&self) -> Size;
    fn ppi(&self) -> u16;
    fn pixel_format(&self) -> ImageFormat;

    fn rotation(&self) -> Rotation {
        Rotation::R0
    }

    fn rotations(&self) -> Rotations {
        Rotations::R0
    }

    fn alignment(&self) -> u16 {
        1
    }

    fn frame_buffers(&self) -> Option<&Self::FrameBuffers> {
        None
    }

    fn frame_buffers_mut(&mut self) -> Option<&mut Self::FrameBuffers> {
        None
    }

    fn gram(&self) -> Option<&Self::Gram> {
        None
    }

    fn epd(&self) -> Option<&Self::Epd> {
        None
    }

    /// `area` is inside the panel, `image` its size in the panel's format.
    /// Only called on a panel without framebuffers.
    fn draw(&self, _area: Rect, _image: &Image) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn set_brightness(&self, _level: u8) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn reconfigure(&mut self, _format: ImageFormat, _rotation: Rotation) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }
}

pub(crate) trait FrameBufferDriver {
    fn count(&self) -> usize;
    fn frame_buffer(&self, index: usize) -> Option<FrameBuffer>;
    fn present(&self, index: usize) -> Result<()>;
    /// The framebuffer on the panel.
    fn shown(&self) -> Option<FrameBuffer>;
    fn scanning(&self) -> Option<usize>;
    fn wait_frame(&self) -> Result<()>;
    fn set_count(&mut self, count: u8) -> Result<()>;
}

pub(crate) trait GramDriver {
    /// `area` is inside the panel and `pixels` holds its rows back to back.
    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket>;
    fn wait(&self, ticket: Ticket) -> Result<()>;
}

pub(crate) trait EpdDriver {
    fn set_mode(&self, mode: EpdMode) -> Result<()>;
    /// `area` is inside the panel.
    fn refresh(&self, area: Rect, waveform: Waveform, all: bool) -> Result<()>;
    /// `area` is inside the panel and on its alignment.
    fn clear(&self, area: Rect) -> Result<()>;
    fn refresh_unit(&self) -> RefreshUnit;
    /// `area` is inside the panel.
    fn busy(&self, area: Rect) -> Result<bool>;
    fn wait_idle(&self) -> Result<()>;
}

impl Driver for Absent {
    type FrameBuffers = Absent;
    type Gram = Absent;
    type Epd = Absent;

    fn size(&self) -> Size {
        match *self {}
    }

    fn ppi(&self) -> u16 {
        match *self {}
    }

    fn pixel_format(&self) -> ImageFormat {
        match *self {}
    }
}

impl FrameBufferDriver for Absent {
    fn count(&self) -> usize {
        match *self {}
    }

    fn frame_buffer(&self, _index: usize) -> Option<FrameBuffer> {
        match *self {}
    }

    fn present(&self, _index: usize) -> Result<()> {
        match *self {}
    }

    fn shown(&self) -> Option<FrameBuffer> {
        match *self {}
    }

    fn scanning(&self) -> Option<usize> {
        match *self {}
    }

    fn wait_frame(&self) -> Result<()> {
        match *self {}
    }

    fn set_count(&mut self, _count: u8) -> Result<()> {
        match *self {}
    }
}

impl GramDriver for Absent {
    unsafe fn write(&self, _area: Rect, _pixels: &[u8]) -> Result<Ticket> {
        match *self {}
    }

    fn wait(&self, _ticket: Ticket) -> Result<()> {
        match *self {}
    }
}

impl EpdDriver for Absent {
    fn set_mode(&self, _mode: EpdMode) -> Result<()> {
        match *self {}
    }

    fn refresh(&self, _area: Rect, _waveform: Waveform, _all: bool) -> Result<()> {
        match *self {}
    }

    fn clear(&self, _area: Rect) -> Result<()> {
        match *self {}
    }

    fn refresh_unit(&self) -> RefreshUnit {
        match *self {}
    }

    fn busy(&self, _area: Rect) -> Result<bool> {
        match *self {}
    }

    fn wait_idle(&self) -> Result<()> {
        match *self {}
    }
}

type BoardDriver = crate::board::Display;

fn pack(size: Size) -> u32 {
    u32::from(size.width) << 16 | u32::from(size.height)
}

pub struct Display {
    size: AtomicU32,
    driver: UnsafeCell<BoardDriver>,
}

unsafe impl Sync for Display {}

impl Display {
    #[allow(dead_code)]
    pub(crate) fn new(driver: BoardDriver) -> Self {
        Self { size: AtomicU32::new(pack(driver.size())), driver: UnsafeCell::new(driver) }
    }

    pub(crate) fn driver(&self) -> &BoardDriver {
        unsafe { &*self.driver.get() }
    }

    /// The logical picture, after the rotation.
    pub fn size(&self) -> Size {
        let size = self.size.load(Ordering::Relaxed);
        Size::new((size >> 16) as u16, size as u16)
    }

    /// Pixels per inch of the panel, the same along both axes.
    pub fn ppi(&self) -> u16 {
        self.driver().ppi()
    }

    pub fn pixel_format(&self) -> ImageFormat {
        self.driver().pixel_format()
    }

    /// How the panel turns the picture it is given.
    pub fn rotation(&self) -> Rotation {
        self.driver().rotation()
    }

    /// What `reconfigure` takes for the rotation.
    pub fn rotations(&self) -> Rotations {
        self.driver().rotations()
    }

    /// The origin and size of what `draw` and `Gram::write` take are multiples
    /// of this, along both axes.
    pub fn alignment(&self) -> u16 {
        self.driver().alignment()
    }

    /// The framebuffers the panel scans out, when it has them.
    pub fn frame_buffers(&self) -> Option<FrameBuffers<'_>> {
        self.driver().frame_buffers().map(|_| FrameBuffers(self))
    }

    /// The panel's own memory, when it takes rectangles into it.
    pub fn gram(&self) -> Option<Gram<'_>> {
        self.driver().gram().map(|_| Gram(self))
    }

    /// The electrophoretic glass, whose picture changes only when driven.
    pub fn epd(&self) -> Option<Epd<'_>> {
        self.driver().epd().map(|_| Epd(self))
    }

    /// Puts `image`, in the panel's format, at `area` of what the panel shows.
    /// Tasks drawing to overlapping areas at the same time is the caller's to
    /// prevent.
    pub unsafe fn draw(&self, area: Rect, image: &Image) -> Result<()> {
        self.check(area, image.format())?;
        if i32::from(image.width()) != i32::from(area.w) || i32::from(image.height()) != i32::from(area.h) {
            return Err(Error::INVALID_ARG);
        }
        let Some(frame_buffers) = self.driver().frame_buffers() else {
            return self.driver().draw(area, image);
        };
        let fb = frame_buffers.shown().ok_or(Error::INVALID_STATE)?;
        let format = fb.format();
        let (start, len) = (format.row_bytes(area.x as usize), format.row_bytes(area.w as usize));
        for (j, y) in (area.y..area.bottom()).enumerate() {
            let src = &image.pixels()[j * image.stride_bytes()..][..len];
            let row = unsafe { fb.row_mut(y as usize) };
            row[start..start + len].copy_from_slice(src);
        }
        Ok(())
    }

    fn check(&self, area: Rect, format: ImageFormat) -> Result<()> {
        if format != self.pixel_format() {
            return Err(Error::NOT_SUPPORTED);
        }
        let size = self.size();
        let (x, y, w, h) = unsigned(area).ok_or(Error::INVALID_ARG)?;
        let bits = format.bits() as u32;
        if x + w > u32::from(size.width) || y + h > u32::from(size.height) || x * bits % 8 != 0 || w * bits % 8 != 0 {
            return Err(Error::INVALID_ARG);
        }
        let align = u32::from(self.alignment());
        if [x, y, w, h].iter().any(|v| v % align != 0) {
            return Err(Error::INVALID_ARG);
        }
        Ok(())
    }

    /// 0..=100; 0 turns the backlight off.
    pub fn set_brightness(&self, level: u8) -> Result<()> {
        self.driver().set_brightness(level)
    }

    /// Rebuilds the panel with `pixel_format` and `rotation`, where `None`
    /// keeps the current value. On failure the previous configuration is
    /// rebuilt if it can be. Either way every framebuffer handed out before is
    /// gone. Nothing but `size` may be called on this display meanwhile.
    pub unsafe fn reconfigure(&self, pixel_format: Option<ImageFormat>, rotation: Option<Rotation>) -> Result<()> {
        let driver = unsafe { &mut *self.driver.get() };
        let format = pixel_format.unwrap_or_else(|| driver.pixel_format());
        let rotation = rotation.unwrap_or_else(|| driver.rotation());
        let result = driver.reconfigure(format, rotation);
        self.size.store(pack(driver.size()), Ordering::Relaxed);
        result
    }
}

/// The framebuffers of a [`Display`] that scans them out.
#[derive(Clone, Copy)]
pub struct FrameBuffers<'a>(&'a Display);

impl<'a> FrameBuffers<'a> {
    fn driver(self) -> &'a <BoardDriver as Driver>::FrameBuffers {
        match self.0.driver().frame_buffers() {
            Some(driver) => driver,
            None => unreachable!(),
        }
    }

    pub fn display(self) -> &'a Display {
        self.0
    }

    pub fn count(self) -> usize {
        self.driver().count()
    }

    pub fn frame_buffer(self, index: usize) -> Option<FrameBuffer> {
        self.driver().frame_buffer(index)
    }

    pub fn present(self, index: usize) -> Result<()> {
        self.driver().present(index)
    }

    /// The framebuffer the panel is scanning; `present` takes effect at the
    /// next frame boundary.
    pub fn scanning(self) -> Option<usize> {
        self.driver().scanning()
    }

    /// Blocks until the next frame boundary.
    pub fn wait_frame(self) -> Result<()> {
        self.driver().wait_frame()
    }

    /// Rebuilds the panel with `count` framebuffers, the same way as
    /// `Display::reconfigure` and under the same terms.
    pub unsafe fn set_count(self, count: u8) -> Result<()> {
        if count == 0 {
            return Err(Error::INVALID_ARG);
        }
        let driver = unsafe { &mut *self.0.driver.get() };
        match driver.frame_buffers_mut() {
            Some(frame_buffers) => frame_buffers.set_count(count),
            None => unreachable!(),
        }
    }

    /// Reads what the panel shows, in its pixel format. Rows may be torn by
    /// concurrent drawing, and only one `Readback` may exist at a time.
    pub unsafe fn readback(self, area: Rect) -> Result<Readback<'a>> {
        let fb = self.driver().shown().ok_or(Error::INVALID_STATE)?;
        let size = fb.size();
        let (x, y, w, h) = unsigned(area).ok_or(Error::INVALID_ARG)?;
        if x + w > u32::from(size.width) || y + h > u32::from(size.height) {
            return Err(Error::INVALID_ARG);
        }
        Ok(Readback { fb, area, next_row: 0, _display: PhantomData })
    }
}

/// The memory of a [`Display`] that keeps the picture itself.
#[derive(Clone, Copy)]
pub struct Gram<'a>(&'a Display);

impl<'a> Gram<'a> {
    fn driver(self) -> &'a <BoardDriver as Driver>::Gram {
        match self.0.driver().gram() {
            Some(driver) => driver,
            None => unreachable!(),
        }
    }

    pub fn display(self) -> &'a Display {
        self.0
    }

    /// Queues `pixels`, `area`'s rows back to back in the panel's format, and
    /// returns without waiting for them to be sent. They must stay unchanged
    /// until `wait` for the ticket returns.
    pub unsafe fn write(self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        let format = self.0.pixel_format();
        self.0.check(area, format)?;
        if pixels.len() != format.row_bytes(area.w as usize) * area.h as usize {
            return Err(Error::INVALID_ARG);
        }
        unsafe { self.driver().write(area, pixels) }
    }

    /// Blocks until what `write` queued for `ticket` has been sent.
    pub fn wait(self, ticket: Ticket) -> Result<()> {
        self.driver().wait(ticket)
    }
}

/// The glass of a [`Display`] that keeps its picture unpowered.
#[derive(Clone, Copy)]
pub struct Epd<'a>(&'a Display);

impl<'a> Epd<'a> {
    fn driver(self) -> &'a <BoardDriver as Driver>::Epd {
        match self.0.driver().epd() {
            Some(driver) => driver,
            None => unreachable!(),
        }
    }

    pub fn display(self) -> &'a Display {
        self.0
    }

    /// Stays until changed; the panel starts in `Hold`.
    pub fn set_mode(self, mode: EpdMode) -> Result<()> {
        self.driver().set_mode(mode)
    }

    /// Drives `area` from what the panel's memory holds, without changing the
    /// mode. `all` drives every pixel of it, not only the changed ones, which
    /// clears ghosting. Returns before the glass settles.
    pub fn refresh(self, area: Rect, waveform: Waveform, all: bool) -> Result<()> {
        self.inside(area)?;
        self.driver().refresh(area, waveform, all)
    }

    /// Drives `area` to white, the known start for later drives; the panel's
    /// memory there turns white too. `area` keeps to `Display::alignment`.
    /// Writes to it wait until the glass settles.
    pub fn clear(self, area: Rect) -> Result<()> {
        self.0.check(area, self.0.pixel_format())?;
        self.driver().clear(area)
    }

    pub fn refresh_unit(self) -> RefreshUnit {
        self.driver().refresh_unit()
    }

    /// Whether what is written to `area` now would wait to reach the glass,
    /// beyond the end of a frame being driven.
    pub fn busy(self, area: Rect) -> Result<bool> {
        self.inside(area)?;
        self.driver().busy(area)
    }

    fn inside(self, area: Rect) -> Result<()> {
        let size = self.0.size();
        let (x, y, w, h) = unsigned(area).ok_or(Error::INVALID_ARG)?;
        if x + w > u32::from(size.width) || y + h > u32::from(size.height) {
            return Err(Error::INVALID_ARG);
        }
        Ok(())
    }

    /// Blocks until every draw, write and refresh has reached the glass.
    pub fn wait_idle(self) -> Result<()> {
        self.driver().wait_idle()
    }
}

/// Lent rows of a [`Readback`]: `height` rows `stride` bytes apart, from panel
/// row `y`.
pub struct Rows<'a> {
    pub y: u16,
    pub height: u16,
    pub stride: usize,
    pub data: &'a [u8],
}

pub struct Readback<'a> {
    fb: FrameBuffer,
    area: Rect,
    next_row: u16,
    _display: PhantomData<&'a Display>,
}

impl Readback<'_> {
    pub fn next(&mut self) -> Result<Option<Rows<'_>>> {
        let height = self.area.h as u16 - self.next_row;
        if height == 0 {
            return Ok(None);
        }
        let y = self.area.y as u16 + self.next_row;
        let format = self.fb.format();
        let start = usize::from(y) * self.fb.stride() + format.row_bytes(self.area.x as usize);
        let len = (usize::from(height) - 1) * self.fb.stride() + format.row_bytes(self.area.w as usize);
        self.next_row = self.area.h as u16;
        let data = unsafe { slice::from_raw_parts(self.fb.as_ptr().add(start), len) };
        Ok(Some(Rows { y, height, stride: self.fb.stride(), data }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Display, FrameBuffer, ImageFormat, Readback, Rect, Size};
    use core::marker::PhantomData;
    use core::ptr::NonNull;

    #[test]
    fn readback_lends_the_area_once() {
        let mut memory = [0u8; 4 * 3 * 2];
        for (i, b) in memory.iter_mut().enumerate() {
            *b = i as u8;
        }
        let ptr = NonNull::new(memory.as_mut_ptr()).unwrap();
        let fb = unsafe { FrameBuffer::new(ptr, Size { width: 4, height: 3 }, ImageFormat::Rgb565) };
        let area = Rect::new(1, 1, 2, 2);
        let mut rb: Readback<'_> = Readback { fb, area, next_row: 0, _display: PhantomData::<&Display> };
        let rows = rb.next().unwrap().unwrap();
        assert_eq!((rows.y, rows.height, rows.stride), (1, 2, 8));
        assert_eq!(rows.data.len(), 8 + 4);
        assert_eq!(rows.data[0], 10);
        assert_eq!(rows.data[8], 18);
        assert!(rb.next().unwrap().is_none());
    }
}
