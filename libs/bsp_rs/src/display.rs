/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::marker::PhantomData;
use core::ptr::NonNull;
use core::slice;
#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
use core::time::Duration;

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
use idf::ppa::{self, Srm, SrmOp};
use idf::{Error, Result};

use crate::Absent;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
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

/// Counter-clockwise, from the logical picture to the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
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

    /// `picture` to draw into as into a framebuffer of its size and format.
    pub unsafe fn of(picture: Picture) -> Option<Self> {
        let fb = unsafe { Self::new(NonNull::new(picture.ptr)?, picture.size, picture.format) };
        (fb.len() <= picture.len).then_some(fb)
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

/// Copies pixels into framebuffers with the display's 2D engine. One per task;
/// the engine serves them in turn.
#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
pub struct Blitter {
    srm: Srm,
}

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
impl Blitter {
    /// Queues `src`, packed rows covering `area` of the logical picture, and
    /// returns. `src` must stay valid and `fb` unread until [`Self::wait`] has
    /// returned for it. A rotated copy must span at least 8 rows of `area`:
    /// the engine stops completing shorter ones while the panel scans out.
    pub unsafe fn start(&mut self, fb: &FrameBuffer, rotation: Rotation, area: Rect, src: *const u8) -> Result<()> {
        let format = match fb.format() {
            PixelFormat::Rgb565 => ppa::ColorMode::Rgb565,
            PixelFormat::Rgb888 => ppa::ColorMode::Rgb888,
            PixelFormat::L8 => return Err(Error::NOT_SUPPORTED),
        };
        let size = fb.size();
        let (dst_x, dst_y) = panel_origin(size, rotation, area).ok_or(Error::INVALID_ARG)?;
        let op = SrmOp {
            src,
            src_width: u32::from(area.width),
            src_height: u32::from(area.height),
            src_x: 0,
            src_y: 0,
            block_width: u32::from(area.width),
            block_height: u32::from(area.height),
            src_format: format,
            dst: fb.as_ptr(),
            dst_size: fb.len() as u32,
            dst_width: u32::from(size.width),
            dst_height: u32::from(size.height),
            dst_x,
            dst_y,
            format,
            rotation: rotation.into(),
            scale: 1.0,
        };
        unsafe { self.srm.start(&op) }
    }

    /// Queues `area` of the logical picture in `fb`, as the panel shows it,
    /// scaled by `scale` into `dst` at `at`. `scale` goes in steps of 1/16.
    pub unsafe fn start_from(&mut self, fb: &FrameBuffer, rotation: Rotation, area: Rect, scale: f32, dst: Picture, at: (u16, u16)) -> Result<()> {
        let size = fb.size();
        let (x, y) = panel_origin(size, rotation, area).ok_or(Error::INVALID_ARG)?;
        let (width, height) = match rotation {
            Rotation::R0 | Rotation::R180 => (u32::from(area.width), u32::from(area.height)),
            Rotation::R90 | Rotation::R270 => (u32::from(area.height), u32::from(area.width)),
        };
        let back = match rotation {
            Rotation::R0 => Rotation::R0,
            Rotation::R90 => Rotation::R270,
            Rotation::R180 => Rotation::R180,
            Rotation::R270 => Rotation::R90,
        };
        let source = Picture { ptr: fb.as_ptr(), size, format: fb.format(), len: fb.len() };
        unsafe { self.start_scaled(source, Rect { x: x as u16, y: y as u16, width: width as u16, height: height as u16 }, back, scale, dst, at) }
    }

    /// Queues `block` of `src`, scaled by `scale` and turned as `rotation`
    /// says, into `dst` at `at`. `scale` goes in steps of 1/16.
    pub unsafe fn start_scaled(&mut self, src: Picture, block: Rect, rotation: Rotation, scale: f32, dst: Picture, at: (u16, u16)) -> Result<()> {
        let op = SrmOp {
            src: src.ptr,
            src_width: u32::from(src.size.width),
            src_height: u32::from(src.size.height),
            src_x: u32::from(block.x),
            src_y: u32::from(block.y),
            block_width: u32::from(block.width),
            block_height: u32::from(block.height),
            src_format: color_mode(src.format)?,
            dst: dst.ptr,
            dst_size: dst.len as u32,
            dst_width: u32::from(dst.size.width),
            dst_height: u32::from(dst.size.height),
            dst_x: u32::from(at.0),
            dst_y: u32::from(at.1),
            format: color_mode(dst.format)?,
            rotation: rotation.into(),
            scale,
        };
        unsafe { self.srm.start(&op) }
    }

    /// Queues a `width` x `height` ARGB8888 picture, turned as `rotation` says,
    /// into `dst`, which holds the turned picture and nothing else. The same
    /// rules as [`Self::start`].
    pub unsafe fn start_argb(&mut self, rotation: Rotation, width: u16, height: u16, src: *const u8, dst: *mut u8) -> Result<()> {
        let (dst_width, dst_height) = match rotation {
            Rotation::R0 | Rotation::R180 => (u32::from(width), u32::from(height)),
            Rotation::R90 | Rotation::R270 => (u32::from(height), u32::from(width)),
        };
        let op = SrmOp {
            src,
            src_width: u32::from(width),
            src_height: u32::from(height),
            src_x: 0,
            src_y: 0,
            block_width: u32::from(width),
            block_height: u32::from(height),
            src_format: ppa::ColorMode::Argb8888,
            dst,
            dst_size: dst_width * dst_height * 4,
            dst_width,
            dst_height,
            dst_x: 0,
            dst_y: 0,
            format: ppa::ColorMode::Argb8888,
            rotation: rotation.into(),
            scale: 1.0,
        };
        unsafe { self.srm.start(&op) }
    }

    pub fn pending(&self) -> u32 {
        self.srm.pending()
    }

    /// `false` on timeout, with the copy still pending.
    pub fn wait_timeout(&mut self, timeout: Duration) -> bool {
        self.srm.wait_timeout(timeout)
    }
}

/// Pixels the 2D engine reads or writes: `len` bytes from `ptr`, holding a
/// picture of `size`. A destination is cache-line aligned in address and `len`.
#[derive(Clone, Copy)]
pub struct Picture {
    pub ptr: *mut u8,
    pub size: Size,
    pub format: PixelFormat,
    pub len: usize,
}

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
fn color_mode(format: PixelFormat) -> Result<ppa::ColorMode> {
    match format {
        PixelFormat::Rgb565 => Ok(ppa::ColorMode::Rgb565),
        PixelFormat::Rgb888 => Ok(ppa::ColorMode::Rgb888),
        PixelFormat::L8 => Err(Error::NOT_SUPPORTED),
    }
}

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
impl From<Rotation> for ppa::Rotation {
    fn from(rotation: Rotation) -> Self {
        match rotation {
            Rotation::R0 => Self::R0,
            Rotation::R90 => Self::R90,
            Rotation::R180 => Self::R180,
            Rotation::R270 => Self::R270,
        }
    }
}

/// Where `area` of the logical picture lands on a panel of `size`.
fn panel_origin(size: Size, rotation: Rotation, area: Rect) -> Option<(u32, u32)> {
    let (panel_w, panel_h) = (u32::from(size.width), u32::from(size.height));
    let (x, y, w, h) = (u32::from(area.x), u32::from(area.y), u32::from(area.width), u32::from(area.height));
    let (logical_w, logical_h) = match rotation {
        Rotation::R0 | Rotation::R180 => (panel_w, panel_h),
        Rotation::R90 | Rotation::R270 => (panel_h, panel_w),
    };
    if x + w > logical_w || y + h > logical_h {
        return None;
    }
    Some(match rotation {
        Rotation::R0 => (x, y),
        Rotation::R90 => (y, panel_h - x - w),
        Rotation::R180 => (panel_w - x - w, panel_h - y - h),
        Rotation::R270 => (panel_w - y - h, x),
    })
}

/// Lays ARGB8888 pictures over framebuffers with the display's 2D engine. One
/// per task.
#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
pub struct Blender {
    blend: ppa::Blend,
}

#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
impl Blender {
    /// `max_pending` tints may be in flight at once.
    pub fn new(max_pending: u32) -> Result<Self> {
        Ok(Self { blend: ppa::Blend::new(max_pending)? })
    }

    /// Queues `area` of the logical picture from `backdrop`, a picture of the
    /// panel as it shows it, under `color` at `alpha` into the same place in
    /// `fb`, and returns. The engine reads `mask`, one byte per pixel of `area`,
    /// and ignores what they hold. All stay in use until
    /// [`Self::wait_timeout`] has returned for it.
    pub unsafe fn start_tint(&mut self, backdrop: &FrameBuffer, fb: &FrameBuffer, rotation: Rotation, area: Rect, color: [u8; 3], alpha: u8, mask: *const u8) -> Result<()> {
        if backdrop.size() != fb.size() {
            return Err(Error::INVALID_ARG);
        }
        let out = Picture { ptr: fb.as_ptr(), size: fb.size(), format: fb.format(), len: fb.len() };
        let (x, y) = panel_origin(fb.size(), rotation, area).ok_or(Error::INVALID_ARG)?;
        unsafe { self.tint(backdrop, rotation, area, out, (x, y), color, alpha, mask) }
    }

    /// The same into `out` at its corner, as the panel shows it: turned
    /// against the logical picture unless `rotation` is `R0`.
    pub unsafe fn start_tint_to(&mut self, backdrop: &FrameBuffer, rotation: Rotation, area: Rect, out: Picture, color: [u8; 3], alpha: u8, mask: *const u8) -> Result<()> {
        unsafe { self.tint(backdrop, rotation, area, out, (0, 0), color, alpha, mask) }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn tint(&mut self, backdrop: &FrameBuffer, rotation: Rotation, area: Rect, out: Picture, at: (u32, u32), color: [u8; 3], alpha: u8, mask: *const u8) -> Result<()> {
        let format = color_mode(backdrop.format())?;
        if out.format != backdrop.format() {
            return Err(Error::INVALID_ARG);
        }
        let size = backdrop.size();
        let (x, y) = panel_origin(size, rotation, area).ok_or(Error::INVALID_ARG)?;
        let (width, height) = match rotation {
            Rotation::R0 | Rotation::R180 => (u32::from(area.width), u32::from(area.height)),
            Rotation::R90 | Rotation::R270 => (u32::from(area.height), u32::from(area.width)),
        };
        let op = ppa::TintOp {
            backdrop: backdrop.as_ptr(),
            backdrop_width: u32::from(size.width),
            backdrop_height: u32::from(size.height),
            x,
            y,
            out: out.ptr,
            out_size: out.len as u32,
            out_width: u32::from(out.size.width),
            out_height: u32::from(out.size.height),
            out_x: at.0,
            out_y: at.1,
            width,
            height,
            format,
            color,
            alpha,
            mask,
        };
        unsafe { self.blend.start_tint(&op) }
    }

    pub fn pending(&self) -> u32 {
        self.blend.pending()
    }

    /// `false` on timeout, with the tint still pending.
    pub fn wait_timeout(&mut self, timeout: Duration) -> bool {
        self.blend.wait_timeout(timeout)
    }

    /// Blocks until done. `src` holds `area` of the logical picture, already
    /// turned as `rotation` says.
    pub unsafe fn blend(&mut self, fb: &FrameBuffer, rotation: Rotation, area: Rect, src: *const u8, timeout: Duration) -> Result<bool> {
        let format = match fb.format() {
            PixelFormat::Rgb565 => ppa::ColorMode::Rgb565,
            PixelFormat::Rgb888 => ppa::ColorMode::Rgb888,
            PixelFormat::L8 => return Err(Error::NOT_SUPPORTED),
        };
        let size = fb.size();
        let (x, y) = panel_origin(size, rotation, area).ok_or(Error::INVALID_ARG)?;
        let (width, height) = match rotation {
            Rotation::R0 | Rotation::R180 => (u32::from(area.width), u32::from(area.height)),
            Rotation::R90 | Rotation::R270 => (u32::from(area.height), u32::from(area.width)),
        };
        let op = ppa::BlendOp {
            foreground: src,
            width,
            height,
            picture: fb.as_ptr(),
            picture_size: fb.len() as u32,
            picture_width: u32::from(size.width),
            picture_height: u32::from(size.height),
            x,
            y,
            format,
        };
        unsafe { self.blend.run(&op, timeout) }
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

    /// The framebuffer the panel is scanning; `present` takes effect at the
    /// next frame boundary.
    pub fn scanning(&self) -> Option<usize> {
        self.0.scanning()
    }

    /// Blocks until the next frame boundary.
    pub fn wait_frame(&self) -> Result<()> {
        self.0.wait_frame()
    }

    /// `max_pending` copies may be in flight at once.
    #[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
    pub fn blitter(&self, max_pending: u32) -> Result<Blitter> {
        Ok(Blitter { srm: Srm::new(max_pending)? })
    }

    /// 0..=100; 0 turns the backlight off.
    pub fn set_brightness(&self, level: u8) -> Result<()> {
        self.0.set_brightness(level)
    }

    /// Reads what the panel shows, in its pixel format. Rows may be torn by
    /// concurrent drawing, and only one `Readback` may exist at a time.
    pub unsafe fn readback(&self, area: Rect) -> Result<Readback<'_>> {
        let fb = self.0.shown().ok_or(Error::NOT_SUPPORTED)?;
        let size = fb.size();
        if u32::from(area.x) + u32::from(area.width) > u32::from(size.width)
            || u32::from(area.y) + u32::from(area.height) > u32::from(size.height)
        {
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
        let height = self.area.height - self.next_row;
        if height == 0 {
            return Ok(None);
        }
        let y = self.area.y + self.next_row;
        let bytes = self.fb.format().bytes();
        let start = usize::from(y) * self.fb.stride() + usize::from(self.area.x) * bytes;
        let len = (usize::from(height) - 1) * self.fb.stride() + usize::from(self.area.width) * bytes;
        self.next_row = self.area.height;
        let data = unsafe { slice::from_raw_parts(self.fb.as_ptr().add(start), len) };
        Ok(Some(Rows { y, height, stride: self.fb.stride(), data }))
    }
}

#[cfg(test)]
mod tests {
    use super::{Display, FrameBuffer, PixelFormat, Readback, Rect, Size};
    use core::marker::PhantomData;
    use core::ptr::NonNull;

    #[test]
    fn readback_lends_the_area_once() {
        let mut memory = [0u8; 4 * 3 * 2];
        for (i, b) in memory.iter_mut().enumerate() {
            *b = i as u8;
        }
        let ptr = NonNull::new(memory.as_mut_ptr()).unwrap();
        let fb = unsafe { FrameBuffer::new(ptr, Size { width: 4, height: 3 }, PixelFormat::Rgb565) };
        let area = Rect { x: 1, y: 1, width: 2, height: 2 };
        let mut rb: Readback<'_> = Readback { fb, area, next_row: 0, _display: PhantomData::<&Display> };
        let rows = rb.next().unwrap().unwrap();
        assert_eq!((rows.y, rows.height, rows.stride), (1, 2, 8));
        assert_eq!(rows.data.len(), 8 + 4);
        assert_eq!(rows.data[0], 10);
        assert_eq!(rows.data[8], 18);
        assert!(rb.next().unwrap().is_none());
    }
}
