/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The codec on the CPU through devkit_image: same layout as the hardware,
 * not the same pixels. Frames finish on submit.
 */

use alloc::vec;
use alloc::vec::Vec;
use core::task::{Context, Poll};
use core::time::Duration;

use devkit_gfx::ImageFormat;
use devkit_image::{DecodeOptions, JpegDecoder, SliceStream};
use idf::{Error, Result};

use super::header::Header;
use super::{Frame, Output};
use crate::wait;

pub(super) fn init() -> Result<()> {
    Ok(())
}

pub(super) struct Session {
    header: Header,
    output: Output,
    row_bytes: usize,
    picture: Vec<u8>,
    next: u16,
    pub blocking: bool,
}

fn convert(src: &[u8], dst: &mut [u8], format: ImageFormat, gray: bool) {
    let pixels = if gray { src.len() } else { src.len() / 3 };
    for i in 0..pixels {
        let (r, g, b) = if gray { (src[i], src[i], src[i]) } else { (src[3 * i], src[3 * i + 1], src[3 * i + 2]) };
        match format {
            ImageFormat::Rgb888 => dst[3 * i..3 * i + 3].copy_from_slice(&[r, g, b]),
            ImageFormat::Bgr888 => dst[3 * i..3 * i + 3].copy_from_slice(&[b, g, r]),
            ImageFormat::Rgb565 => {
                let v = u16::from(r >> 3) << 11 | u16::from(g >> 2) << 5 | u16::from(b >> 3);
                dst[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
            }
            _ => dst[i] = r,
        }
    }
}

impl Session {
    pub(super) fn new(frame: Frame<'_>) -> Result<Self> {
        let h = &frame.header;
        let mut decoder = JpegDecoder::open(SliceStream::new(frame.data), &DecodeOptions::default()).map_err(|_| Error::INVALID_ARG)?;
        let info = decoder.info();
        let gray = info.format == ImageFormat::L8;
        let mut row = vec![0u8; info.row_bytes()];
        let mut picture = vec![0u8; frame.row_bytes * usize::from(h.padded_height)];
        let used = frame.config.format.row_bytes(usize::from(info.width));
        for y in 0..usize::from(info.height) {
            if !decoder.next_row(&mut row).map_err(|_| Error::INVALID_ARG)? {
                break;
            }
            convert(&row, &mut picture[y * frame.row_bytes..y * frame.row_bytes + used], frame.config.format, gray);
        }
        if let Output::Whole(ptr, _) = frame.output {
            unsafe { core::ptr::copy_nonoverlapping(picture.as_ptr(), ptr, picture.len()) };
        }
        Ok(Self { header: frame.header, output: frame.output, row_bytes: frame.row_bytes, picture, next: 0, blocking: false })
    }

    pub(super) fn row_bytes(&self) -> usize {
        self.row_bytes
    }

    pub(super) fn slice(&self) -> Duration {
        Duration::from_millis(1)
    }

    pub(super) fn release(&mut self, _index: u16) {}
}

impl wait::Pollable for Session {
    type Output = Result<Option<u16>>;

    fn poll(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Option<u16>>> {
        let Output::Strips(l) = self.output else { return Poll::Ready(Ok(None)) };
        if self.next >= l.strip_count {
            return Poll::Ready(Ok(None));
        }
        let index = self.next;
        self.next += 1;
        let y = usize::from(index * l.rows);
        let rows = usize::from(l.rows.min(self.header.padded_height - index * l.rows));
        let src = &self.picture[y * self.row_bytes..(y + rows) * self.row_bytes];
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), l.buffers[usize::from(index) % l.count], src.len()) };
        Poll::Ready(Ok(Some(index)))
    }

    fn forget_waker(&mut self) {}
}
