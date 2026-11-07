/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::slice;
use idf::{Error, Result};

use crate::Absent;

pub use devkit_gfx::{ImageFormat, Rect, Rotation, Size};
use devkit_gfx::Target;

#[derive(Default)]
#[non_exhaustive]
pub struct Config {
    /// 0 selects the board default.
    pub fb_num: u8,
    /// `None` selects the board default.
    pub pixel_format: Option<ImageFormat>,
}

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

pub(crate) trait Driver: Send + Sync {
    fn size(&self) -> Size;
    fn pixel_format(&self) -> ImageFormat;

    fn frame_buffer_count(&self) -> usize {
        0
    }

    fn frame_buffer(&self, _index: usize) -> Option<FrameBuffer> {
        None
    }

    fn present(&self, _index: usize) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    /// The framebuffer on the panel.
    fn shown(&self) -> Option<FrameBuffer> {
        None
    }

    fn scanning(&self) -> Option<usize> {
        None
    }

    fn wait_frame(&self) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn set_brightness(&self, _level: u8) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn reconfigure(&mut self, _format: ImageFormat, _fb_num: u8) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }
}

impl Driver for Absent {
    fn size(&self) -> Size {
        match *self {}
    }

    fn pixel_format(&self) -> ImageFormat {
        match *self {}
    }
}

#[cfg_attr(not(target_os = "none"), allow(unused_macros))]
macro_rules! driver_enum {
    ($vis:vis enum $name:ident { $($variant:ident($ty:ty)),+ $(,)? }) => {
        $vis enum $name {
            $($variant($ty)),+
        }

        impl $crate::display::Driver for $name {
            fn size(&self) -> $crate::display::Size {
                match self { $(Self::$variant(d) => d.size()),+ }
            }

            fn pixel_format(&self) -> $crate::display::ImageFormat {
                match self { $(Self::$variant(d) => d.pixel_format()),+ }
            }

            fn frame_buffer_count(&self) -> usize {
                match self { $(Self::$variant(d) => d.frame_buffer_count()),+ }
            }

            fn frame_buffer(&self, index: usize) -> Option<$crate::display::FrameBuffer> {
                match self { $(Self::$variant(d) => d.frame_buffer(index)),+ }
            }

            fn present(&self, index: usize) -> idf::Result<()> {
                match self { $(Self::$variant(d) => d.present(index)),+ }
            }

            fn shown(&self) -> Option<$crate::display::FrameBuffer> {
                match self { $(Self::$variant(d) => d.shown()),+ }
            }

            fn scanning(&self) -> Option<usize> {
                match self { $(Self::$variant(d) => d.scanning()),+ }
            }

            fn wait_frame(&self) -> idf::Result<()> {
                match self { $(Self::$variant(d) => d.wait_frame()),+ }
            }

            fn set_brightness(&self, level: u8) -> idf::Result<()> {
                match self { $(Self::$variant(d) => d.set_brightness(level)),+ }
            }

            fn reconfigure(&mut self, format: $crate::display::ImageFormat, fb_num: u8) -> idf::Result<()> {
                match self { $(Self::$variant(d) => d.reconfigure(format, fb_num)),+ }
            }
        }
    };
}

#[cfg_attr(not(target_os = "none"), allow(unused_imports))]
pub(crate) use driver_enum;

pub struct Display {
    size: Size,
    driver: UnsafeCell<crate::board::Display>,
}

unsafe impl Sync for Display {}

impl Display {
    #[allow(dead_code)]
    pub(crate) fn new(driver: crate::board::Display) -> Self {
        Self { size: driver.size(), driver: UnsafeCell::new(driver) }
    }

    fn driver(&self) -> &crate::board::Display {
        unsafe { &*self.driver.get() }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn pixel_format(&self) -> ImageFormat {
        self.driver().pixel_format()
    }

    pub fn frame_buffer_count(&self) -> usize {
        self.driver().frame_buffer_count()
    }

    pub fn frame_buffer(&self, index: usize) -> Option<FrameBuffer> {
        self.driver().frame_buffer(index)
    }

    pub fn present(&self, index: usize) -> Result<()> {
        self.driver().present(index)
    }

    /// The framebuffer the panel is scanning; `present` takes effect at the
    /// next frame boundary.
    pub fn scanning(&self) -> Option<usize> {
        self.driver().scanning()
    }

    /// Blocks until the next frame boundary.
    pub fn wait_frame(&self) -> Result<()> {
        self.driver().wait_frame()
    }

    /// `max_pending` copies may be in flight at once.
    /// 0..=100; 0 turns the backlight off.
    pub fn set_brightness(&self, level: u8) -> Result<()> {
        self.driver().set_brightness(level)
    }

    /// Rebuilds the panel with `config`, where 0 and `None` keep the current
    /// value. On failure the previous configuration is rebuilt if it can be.
    /// Either way every framebuffer handed out before is gone. Nothing but
    /// `size` may be called on this display meanwhile.
    pub unsafe fn reconfigure(&self, config: &Config) -> Result<()> {
        let driver = unsafe { &mut *self.driver.get() };
        let format = config.pixel_format.unwrap_or_else(|| driver.pixel_format());
        let fb_num = if config.fb_num != 0 { config.fb_num } else { driver.frame_buffer_count() as u8 };
        driver.reconfigure(format, fb_num)
    }

    /// Reads what the panel shows, in its pixel format. Rows may be torn by
    /// concurrent drawing, and only one `Readback` may exist at a time.
    pub unsafe fn readback(&self, area: Rect) -> Result<Readback<'_>> {
        let fb = self.driver().shown().ok_or(Error::NOT_SUPPORTED)?;
        let size = fb.size();
        let (x, y, w, h) = unsigned(area).ok_or(Error::INVALID_ARG)?;
        if x + w > u32::from(size.width) || y + h > u32::from(size.height) {
            return Err(Error::INVALID_ARG);
        }
        Ok(Readback { fb, area, next_row: 0, _display: PhantomData })
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
