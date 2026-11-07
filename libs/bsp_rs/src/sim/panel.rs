/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::ffi::CStr;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

use idf::heap::Caps;
use idf::log::error;
use idf::{Error, Result, thread};
use super::Host;
use super::protocol::{self, Event};
use crate::display::{Driver, FrameBuffer, ImageFormat, Size};

const TAG: &CStr = c"sim_panel";
const MAX_FRAME_BUFFERS: usize = 3;
const FRAME_PERIOD: Duration = Duration::from_millis(16);

pub(crate) struct SimPanel {
    host: &'static Host,
    size: Size,
    formats: &'static [ImageFormat],
    format: ImageFormat,
    buffers: Vec<Vec<u8, Caps>>,
    shown: AtomicUsize,
}

impl SimPanel {
    /// Framebuffers live in the PSRAM region the host shows them from.
    pub(crate) fn new(host: &'static Host, size: Size, formats: &'static [ImageFormat], format: ImageFormat, fb_num: u8) -> Result<Self> {
        let buffers = allocate(size, formats, format, fb_num)?;
        Ok(Self { host, size, formats, format, buffers, shown: AtomicUsize::new(0) })
    }
}

fn allocate(size: Size, formats: &[ImageFormat], format: ImageFormat, fb_num: u8) -> Result<Vec<Vec<u8, Caps>>> {
    let fb_num = usize::from(fb_num.max(1));
    if fb_num > MAX_FRAME_BUFFERS {
        return Err(Error::INVALID_ARG);
    }
    if !formats.contains(&format) || wire_format(format).is_none() {
        return Err(Error::NOT_SUPPORTED);
    }
    let len = format.row_bytes(usize::from(size.width)) * usize::from(size.height);
    let mut buffers = Vec::with_capacity(fb_num);
    for _ in 0..fb_num {
        let mut fb = Vec::new_in(Caps::SPIRAM);
        fb.try_reserve_exact(len).map_err(|_| Error::NO_MEM)?;
        fb.resize(len, 0);
        buffers.push(fb);
    }
    Ok(buffers)
}

impl Driver for SimPanel {
    fn size(&self) -> Size {
        self.size
    }

    fn pixel_format(&self) -> ImageFormat {
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
        self.shown.store(index, Ordering::Relaxed);
        self.host.send(&Event::Present {
            offset,
            width: self.size.width,
            height: self.size.height,
            stride: self.format.row_bytes(usize::from(self.size.width)) as u32,
            format: wire_format(self.format).ok_or(Error::NOT_SUPPORTED)?,
        });
        Ok(())
    }

    fn shown(&self) -> Option<FrameBuffer> {
        self.frame_buffer(self.shown.load(Ordering::Relaxed))
    }

    fn scanning(&self) -> Option<usize> {
        Some(self.shown.load(Ordering::Relaxed))
    }

    fn wait_frame(&self) -> Result<()> {
        thread::sleep(FRAME_PERIOD);
        Ok(())
    }

    fn set_brightness(&self, level: u8) -> Result<()> {
        self.host.send(&Event::Brightness { level: level.min(100) });
        Ok(())
    }

    fn reconfigure(&mut self, format: ImageFormat, fb_num: u8) -> Result<()> {
        if format == self.format && usize::from(fb_num) == self.buffers.len() {
            return Ok(());
        }
        let previous = self.buffers.len() as u8;
        self.buffers = Vec::new();
        self.shown.store(0, Ordering::Relaxed);
        match allocate(self.size, self.formats, format, fb_num) {
            Ok(buffers) => {
                (self.buffers, self.format) = (buffers, format);
                Ok(())
            }
            Err(err) => {
                error!(TAG, "reconfigure to {format:?} x{fb_num}: {err}");
                match allocate(self.size, self.formats, self.format, previous) {
                    Ok(buffers) => self.buffers = buffers,
                    Err(err) => error!(TAG, "restore: {err}"),
                }
                Err(err)
            }
        }
    }
}

fn wire_format(format: ImageFormat) -> Option<protocol::PixelFormat> {
    match format {
        ImageFormat::L8 => Some(protocol::PixelFormat::L8),
        ImageFormat::Rgb565 => Some(protocol::PixelFormat::Rgb565),
        ImageFormat::Bgr888 => Some(protocol::PixelFormat::Bgr888),
        _ => None,
    }
}
