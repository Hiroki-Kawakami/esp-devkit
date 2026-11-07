/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::alloc::Global;
use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::NonNull;
use core::slice;

use crate::format::{IMAGE_DATA_OFFSET, ImageFormat, ImageHeader};

#[derive(Clone, Copy)]
pub struct Image<'a> {
    format: ImageFormat,
    width: u16,
    height: u16,
    stride: usize,
    pixels: &'a [u8],
}

impl Image<'static> {
    /// Reads a resource binary; panics, at compile time for a `static`, when it
    /// is not one.
    pub const fn parse(data: &'static [u8]) -> Self {
        let Some(header) = ImageHeader::read(data) else { panic!("not an image resource") };
        let (_, pixels) = data.split_at(IMAGE_DATA_OFFSET);
        Self {
            format: header.format,
            width: header.width,
            height: header.height,
            stride: header.stride as usize,
            pixels,
        }
    }
}

impl<'a> Image<'a> {
    /// `stride` is in pixels and starts every row on a byte; `pixels` holds
    /// `height` rows of it.
    pub fn from_pixels(format: ImageFormat, width: u16, height: u16, stride: usize, pixels: &'a [u8]) -> Self {
        assert!(stride >= usize::from(width) && stride * format.bits() % 8 == 0);
        assert!(pixels.len() >= format.row_bytes(stride) * usize::from(height));
        Self { format, width, height, stride, pixels }
    }

    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    pub const fn width(&self) -> u16 {
        self.width
    }

    pub const fn height(&self) -> u16 {
        self.height
    }

    pub const fn stride(&self) -> usize {
        self.stride
    }

    pub const fn stride_bytes(&self) -> usize {
        self.format.row_bytes(self.stride)
    }

    pub const fn pixels(&self) -> &'a [u8] {
        self.pixels
    }
}

pub trait ImageSource: Send + Sync {
    fn image(&self) -> Image<'_>;
}

pub struct ImageBuf<A: Allocator = Global> {
    format: ImageFormat,
    width: u16,
    height: u16,
    stride: usize,
    ptr: NonNull<u8>,
    layout: Layout,
    alloc: A,
}

unsafe impl<A: Allocator + Send> Send for ImageBuf<A> {}
unsafe impl<A: Allocator + Sync> Sync for ImageBuf<A> {}

impl ImageBuf {
    pub fn new(format: ImageFormat, width: u16, height: u16, stride: usize) -> Result<Self, AllocError> {
        Self::new_in(format, width, height, stride, 1, Global)
    }
}

impl<A: Allocator> ImageBuf<A> {
    /// Zeroed, starting on `align` and rounded up to a multiple of it.
    pub fn new_in(format: ImageFormat, width: u16, height: u16, stride: usize, align: usize, alloc: A) -> Result<Self, AllocError> {
        let layout = Self::layout(format, width, height, stride, align);
        let ptr = alloc.allocate_zeroed(layout)?.cast();
        Ok(Self { format, width, height, stride, ptr, layout, alloc })
    }

    /// `new_in` without clearing.
    ///
    /// # Safety
    /// Every pixel must be written before it is read.
    pub unsafe fn new_uninit_in(format: ImageFormat, width: u16, height: u16, stride: usize, align: usize, alloc: A) -> Result<Self, AllocError> {
        let layout = Self::layout(format, width, height, stride, align);
        let ptr = alloc.allocate(layout)?.cast();
        Ok(Self { format, width, height, stride, ptr, layout, alloc })
    }

    fn layout(format: ImageFormat, width: u16, height: u16, stride: usize, align: usize) -> Layout {
        assert!(stride >= usize::from(width) && stride * format.bits() % 8 == 0);
        let bytes = format.row_bytes(stride) * usize::from(height);
        Layout::from_size_align(bytes, align).expect("align is a power of two").pad_to_align()
    }

    /// Another size within the same memory; the pixels are not touched.
    pub fn reshape(&mut self, format: ImageFormat, width: u16, height: u16, stride: usize) {
        assert!(stride >= usize::from(width) && stride * format.bits() % 8 == 0);
        assert!(format.row_bytes(stride) * usize::from(height) <= self.layout.size());
        (self.format, self.width, self.height, self.stride) = (format, width, height, stride);
    }

    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    pub const fn width(&self) -> u16 {
        self.width
    }

    pub const fn height(&self) -> u16 {
        self.height
    }

    pub const fn stride(&self) -> usize {
        self.stride
    }

    pub const fn stride_bytes(&self) -> usize {
        self.format.row_bytes(self.stride)
    }

    pub fn pixels(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len()) }
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len()) }
    }

    pub fn image(&self) -> Image<'_> {
        Image::from_pixels(self.format, self.width, self.height, self.stride, self.pixels())
    }

    fn len(&self) -> usize {
        self.stride_bytes() * usize::from(self.height)
    }
}

impl<A: Allocator> Drop for ImageBuf<A> {
    fn drop(&mut self) {
        unsafe { self.alloc.deallocate(self.ptr, self.layout) };
    }
}

impl<A: Allocator + Send + Sync> ImageSource for ImageBuf<A> {
    fn image(&self) -> Image<'_> {
        ImageBuf::image(self)
    }
}
