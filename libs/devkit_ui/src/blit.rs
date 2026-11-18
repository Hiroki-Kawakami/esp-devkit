/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ptr::NonNull;
use core::slice;
use core::time::Duration;

use devkit_gfx::{Image, ImageFormat, Rect, Rotation, Size, Target};
use idf::{Error, Result};

/// `src_rect` of `src`, turned and scaled, to `at` of the target's logical
/// picture, converted to its format.
// Read only by blitters, which a chip without a 2D engine has none of.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) struct Blit<'a> {
    pub(crate) src: Image<'a>,
    pub(crate) src_rect: Rect,
    pub(crate) at: (i16, i16),
    pub(crate) rotation: Rotation,
    pub(crate) scale: f32,
}

impl<'a> Blit<'a> {
    /// All of `src` at the target's top left, as it is.
    pub(crate) fn new(src: Image<'a>) -> Self {
        let src_rect = Rect::new(0, 0, src.width() as i16, src.height() as i16);
        Self { src, src_rect, at: (0, 0), rotation: Rotation::R0, scale: 1.0 }
    }
}

/// `fg_rect` of the ARGB `fg` over what the target holds at `at`.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) struct Blend<'a> {
    pub(crate) fg: Image<'a>,
    pub(crate) fg_rect: Rect,
    pub(crate) at: (i16, i16),
}

/// Copies on a 2D engine, as many in flight as it was made for, finished in
/// the order they went. The pixels of each stay in use until it is waited for.
pub(crate) trait Blitter: Send {
    /// Refused while it is full.
    fn blit(&mut self, target: Target<'static>, op: &Blit<'static>) -> Result<()>;
    /// Refused while it is full.
    fn blend(&mut self, target: Target<'static>, op: &Blend<'static>) -> Result<()>;
    fn pending(&self) -> usize;
    /// Waits for the oldest and gives its outcome; `None` on timeout, with it
    /// still pending, and `Some(Ok(()))` with none pending.
    fn wait_timeout(&mut self, timeout: Duration) -> Option<Result<()>>;
}

/// A blitter that takes up to the given number in flight.
pub(crate) type NewBlitter = fn(usize) -> Result<Box<dyn Blitter>>;

/// Waits for everything queued: the first failure, or `TIMEOUT` when one
/// takes longer than `timeout`.
pub(crate) fn finish(blitter: &mut dyn Blitter, timeout: Duration) -> Result<()> {
    let mut result = Ok(());
    while blitter.pending() > 0 {
        let outcome = blitter.wait_timeout(timeout).ok_or(Error::TIMEOUT)?;
        result = result.and(outcome);
    }
    result
}

/// `size` of `format` packed at `ptr`, for as long as the caller keeps it.
pub(crate) unsafe fn image(ptr: *const u8, format: ImageFormat, size: Size) -> Image<'static> {
    let len = format.row_bytes(usize::from(size.width)) * usize::from(size.height);
    Image::from_pixels(format, size.width, size.height, usize::from(size.width), unsafe { slice::from_raw_parts(ptr, len) })
}

/// The same, written into; nothing else may use the pixels meanwhile.
pub(crate) unsafe fn target(ptr: *mut u8, format: ImageFormat, size: Size) -> Result<Target<'static>> {
    let len = format.row_bytes(usize::from(size.width)) * usize::from(size.height);
    let ptr = NonNull::new(ptr).ok_or(Error::INVALID_ARG)?;
    unsafe { Target::from_raw(ptr, len, format, size, usize::from(size.width)) }.ok_or(Error::INVALID_ARG)
}
