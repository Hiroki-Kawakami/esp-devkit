/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::ptr::NonNull;

use idf::heap::Caps;
use idf::{Error, Result};
use sim_device::Host;
use sim_device::protocol::{self, Event};

use crate::display::{Driver, FrameBuffer, PixelFormat, Size};

const MAX_FRAME_BUFFERS: usize = 3;

pub(crate) struct SimPanel {
    host: &'static Host,
    size: Size,
    format: PixelFormat,
    buffers: Vec<Vec<u8, Caps>>,
}

impl SimPanel {
    /// Framebuffers live in the PSRAM region the host shows them from.
    pub(crate) fn new(host: &'static Host, size: Size, format: PixelFormat, fb_num: u8) -> Result<Self> {
        let fb_num = usize::from(fb_num.max(1));
        if fb_num > MAX_FRAME_BUFFERS {
            return Err(Error::INVALID_ARG);
        }
        let len = usize::from(size.width) * usize::from(size.height) * format.bytes();
        let mut buffers = Vec::with_capacity(fb_num);
        for _ in 0..fb_num {
            let mut fb = Vec::new_in(Caps::SPIRAM);
            fb.try_reserve_exact(len).map_err(|_| Error::NO_MEM)?;
            fb.resize(len, 0);
            buffers.push(fb);
        }
        Ok(Self { host, size, format, buffers })
    }
}

impl Driver for SimPanel {
    fn size(&self) -> Size {
        self.size
    }

    fn pixel_format(&self) -> PixelFormat {
        self.format
    }

    fn frame_buffer_count(&self) -> usize {
        self.buffers.len()
    }

    fn frame_buffer(&self, index: usize) -> Option<FrameBuffer> {
        let ptr = NonNull::new(self.buffers.get(index)?.as_ptr().cast_mut())?;
        Some(unsafe { FrameBuffer::new(ptr, self.size, self.format) })
    }

    fn present(&self, index: usize) -> Result<()> {
        let fb = self.buffers.get(index).ok_or(Error::INVALID_ARG)?;
        let offset = self.host.psram_offset(fb.as_ptr()).ok_or(Error::INVALID_STATE)?;
        self.host.send(&Event::Present {
            offset,
            width: self.size.width,
            height: self.size.height,
            stride: (usize::from(self.size.width) * self.format.bytes()) as u32,
            format: match self.format {
                PixelFormat::L8 => protocol::PixelFormat::L8,
                PixelFormat::Rgb565 => protocol::PixelFormat::Rgb565,
                PixelFormat::Rgb888 => protocol::PixelFormat::Bgr888,
            },
        });
        Ok(())
    }

    fn set_brightness(&self, level: u8) -> Result<()> {
        self.host.send(&Event::Brightness { level: level.min(100) });
        Ok(())
    }
}
