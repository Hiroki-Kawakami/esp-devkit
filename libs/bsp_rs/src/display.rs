/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ptr::NonNull;
use core::slice;

use idf::{Error, Result};

use crate::Absent;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    L8,
    Rgb565,
    /// B, G, R in memory.
    Rgb888,
}

impl PixelFormat {
    pub const fn bytes(self) -> usize {
        match self {
            Self::L8 => 1,
            Self::Rgb565 => 2,
            Self::Rgb888 => 3,
        }
    }
}

#[derive(Default)]
#[non_exhaustive]
pub struct Config {
    /// 0 selects the board default.
    pub fb_num: u8,
    /// `None` selects the board default.
    pub pixel_format: Option<PixelFormat>,
}

#[derive(Clone, Copy)]
pub struct FrameBuffer {
    ptr: NonNull<u8>,
    size: Size,
    stride: usize,
    format: PixelFormat,
}

unsafe impl Send for FrameBuffer {}
unsafe impl Sync for FrameBuffer {}

impl FrameBuffer {
    #[cfg_attr(not(target_os = "none"), allow(dead_code))]
    pub(crate) unsafe fn new(ptr: NonNull<u8>, size: Size, format: PixelFormat) -> Self {
        let stride = usize::from(size.width) * format.bytes();
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

    pub fn format(&self) -> PixelFormat {
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
}

pub(crate) trait Driver: Send + Sync {
    fn size(&self) -> Size;
    fn pixel_format(&self) -> PixelFormat;

    fn frame_buffer_count(&self) -> usize {
        0
    }

    fn frame_buffer(&self, _index: usize) -> Option<FrameBuffer> {
        None
    }

    fn present(&self, _index: usize) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn set_brightness(&self, _level: u8) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }
}

impl Driver for Absent {
    fn size(&self) -> Size {
        match *self {}
    }

    fn pixel_format(&self) -> PixelFormat {
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

            fn pixel_format(&self) -> $crate::display::PixelFormat {
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

            fn set_brightness(&self, level: u8) -> idf::Result<()> {
                match self { $(Self::$variant(d) => d.set_brightness(level)),+ }
            }
        }
    };
}

#[cfg_attr(not(target_os = "none"), allow(unused_imports))]
pub(crate) use driver_enum;

pub struct Display(pub(crate) crate::board::Display);

impl Display {
    pub fn size(&self) -> Size {
        self.0.size()
    }

    pub fn pixel_format(&self) -> PixelFormat {
        self.0.pixel_format()
    }

    pub fn frame_buffer_count(&self) -> usize {
        self.0.frame_buffer_count()
    }

    pub fn frame_buffer(&self, index: usize) -> Option<FrameBuffer> {
        self.0.frame_buffer(index)
    }

    pub fn present(&self, index: usize) -> Result<()> {
        self.0.present(index)
    }

    /// 0..=100; 0 turns the backlight off.
    pub fn set_brightness(&self, level: u8) -> Result<()> {
        self.0.set_brightness(level)
    }
}
