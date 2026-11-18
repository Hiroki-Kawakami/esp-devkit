/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::ptr::NonNull;

use devkit_gfx::{Target, copy};
use idf::heap::Caps;
use idf::sync::Mutex;
use idf::{Error, Result};

use super::Host;
use super::protocol::{self, Event};
use crate::display::{Driver, Image, ImageFormat, Rect, Rotation, Size, Ticket};

/// A panel that keeps the picture in its own memory, turned as the panel
/// would. Writes land at once; the memory is shown in `Rgb565`.
pub(crate) struct SimGram {
    host: &'static Host,
    size: Size,
    format: ImageFormat,
    rotation: Rotation,
    memory: Mutex<Vec<u8, Caps>>,
}

impl SimGram {
    /// `size` is unturned; the memory lives in the PSRAM region the host
    /// shows it from.
    pub(crate) fn new(host: &'static Host, size: Size, format: ImageFormat, rotation: Rotation) -> Result<Self> {
        let len = ImageFormat::Rgb565.row_bytes(usize::from(size.width)) * usize::from(size.height);
        let mut memory = Vec::new_in(Caps::SPIRAM);
        memory.try_reserve_exact(len).map_err(|_| Error::NO_MEM)?;
        memory.resize(len, 0);
        Ok(Self { host, size, format, rotation, memory: Mutex::new(memory) })
    }

    fn put(&self, area: Rect, image: &Image) -> Result<()> {
        let mut memory = self.memory.lock();
        let ptr = NonNull::new(memory.as_mut_ptr()).ok_or(Error::INVALID_STATE)?;
        let stride = usize::from(self.size.width);
        let mut target = unsafe { Target::from_raw(ptr, memory.len(), ImageFormat::Rgb565, self.size, stride) }
            .and_then(|t| t.rotated(self.rotation).crop(area))
            .ok_or(Error::INVALID_ARG)?;
        if !copy(image, &mut target) {
            return Err(Error::NOT_SUPPORTED);
        }
        let offset = self.host.psram_offset(memory.as_ptr()).ok_or(Error::INVALID_STATE)?;
        self.host.send(&Event::Present {
            offset,
            width: self.size.width,
            height: self.size.height,
            stride: ImageFormat::Rgb565.row_bytes(stride) as u32,
            format: protocol::PixelFormat::Rgb565,
        });
        Ok(())
    }
}

impl Driver for SimGram {
    fn size(&self) -> Size {
        if self.rotation.swaps_axes() { Size::new(self.size.height, self.size.width) } else { self.size }
    }

    fn pixel_format(&self) -> ImageFormat {
        self.format
    }

    fn rotation(&self) -> Rotation {
        self.rotation
    }

    unsafe fn write(&self, area: Rect, pixels: &[u8]) -> Result<Ticket> {
        self.put(area, &Image::from_pixels(self.format, area.w as u16, area.h as u16, area.w as usize, pixels))?;
        Ok(Ticket(0))
    }

    fn wait(&self, _ticket: Ticket) -> Result<()> {
        Ok(())
    }

    fn draw(&self, area: Rect, image: &Image) -> Result<()> {
        self.put(area, image)
    }

    fn set_brightness(&self, level: u8) -> Result<()> {
        self.host.send(&Event::Brightness { level: level.min(100) });
        Ok(())
    }

    fn reconfigure(&mut self, format: ImageFormat, fb_num: u8, rotation: Rotation) -> Result<()> {
        if format != self.format || fb_num != 0 {
            return Err(Error::NOT_SUPPORTED);
        }
        self.rotation = rotation;
        Ok(())
    }
}
