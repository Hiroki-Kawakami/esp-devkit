/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::format::ImageFormat;
use crate::geom::{Rect, Rotation, Size};

/// Where a writer puts pixels: `area` of the logical picture, which `rotation`
/// turns onto the stored picture of `size`. Whoever holds it is the only one
/// writing there.
pub struct Target<'a> {
    ptr: NonNull<u8>,
    len: usize,
    size: Size,
    stride: usize,
    format: ImageFormat,
    rotation: Rotation,
    area: Rect,
    _pixels: PhantomData<&'a mut [u8]>,
}

unsafe impl Send for Target<'_> {}

impl<'a> Target<'a> {
    /// `stride` is in pixels and must start every row on a byte; `None` when it
    /// does not, or `pixels` is too short.
    pub fn new(pixels: &'a mut [u8], format: ImageFormat, size: Size, stride: usize) -> Option<Self> {
        let ptr = NonNull::new(pixels.as_mut_ptr())?;
        unsafe { Self::from_raw(ptr, pixels.len(), format, size, stride) }
    }

    /// Nothing else may access the `len` bytes at `ptr` while it lives.
    pub unsafe fn from_raw(ptr: NonNull<u8>, len: usize, format: ImageFormat, size: Size, stride: usize) -> Option<Self> {
        let width = usize::from(size.width);
        if stride < width || stride * format.bits() % 8 != 0 || format.row_bytes(stride) * usize::from(size.height) > len {
            return None;
        }
        let area = Rect::new(0, 0, size.width as i16, size.height as i16);
        Some(Self { ptr, len, size, stride, format, rotation: Rotation::R0, area, _pixels: PhantomData })
    }

    /// The whole logical picture, turned by `rotation`.
    pub fn rotated(self, rotation: Rotation) -> Self {
        let logical = if rotation.swaps_axes() { Size::new(self.size.height, self.size.width) } else { self.size };
        let area = Rect::new(0, 0, logical.width as i16, logical.height as i16);
        Self { rotation, area, ..self }
    }

    /// `None` when `area` leaves the current one.
    pub fn crop(self, area: Rect) -> Option<Self> {
        if area.is_empty() || !self.area.contains(&area) {
            return None;
        }
        Some(Self { area, ..self })
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn stride_bytes(&self) -> usize {
        self.format.row_bytes(self.stride)
    }

    pub fn format(&self) -> ImageFormat {
        self.format
    }

    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    /// Where `rect` of the logical picture lies in the stored one; `None` when
    /// it leaves `area`.
    pub fn stored_rect(&self, rect: Rect) -> Option<Rect> {
        if rect.is_empty() || !self.area.contains(&rect) {
            return None;
        }
        let (width, height) = (self.size.width as i16, self.size.height as i16);
        let Rect { x, y, w, h } = rect;
        Some(match self.rotation {
            Rotation::R0 => rect,
            Rotation::R90 => Rect::new(y, height - x - w, h, w),
            Rotation::R180 => Rect::new(width - x - w, height - y - h, w, h),
            Rotation::R270 => Rect::new(width - y - h, x, h, w),
        })
    }
}
