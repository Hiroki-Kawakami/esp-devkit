/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::{String, ToString};

use bsp_rs::display::{Display, PixelFormat, Rect};
use devkit_image::{Encoder, Error, JpegEncoder, JpegOptions, PixelFormat as ImageFormat};
use idf::heap::Caps;

type Sink<'a> = &'a mut dyn FnMut(&[u8]) -> bool;

/// Encodes what the panel shows, streaming the JPEG to `sink`.
pub(crate) fn encode(display: &Display, quality: u8, sink: Sink) -> Result<(), String> {
    let size = display.size();
    let format = match display.pixel_format() {
        PixelFormat::L8 => ImageFormat::Gray8,
        PixelFormat::Rgb565 => ImageFormat::Rgb565,
        PixelFormat::Rgb888 => ImageFormat::Bgr888,
    };
    let options = JpegOptions { quality, caps: Some(Caps::ACCESS_8BIT), ..Default::default() };
    let write = |data: &[u8]| if sink(data) { Ok(()) } else { Err(Error::Io) };
    let mut encoder = JpegEncoder::new(size.width, size.height, format, &options, write).map_err(encoder_error)?;

    let area = Rect { x: 0, y: 0, width: size.width, height: size.height };
    let mut rows = unsafe { display.readback(area) }.map_err(|e| e.to_string())?;
    while let Some(chunk) = rows.next().map_err(|e| e.to_string())? {
        for r in 0..usize::from(chunk.height) {
            encoder.push_row(&chunk.data[r * chunk.stride..]).map_err(encoder_error)?;
        }
    }
    encoder.finish().map(|_| ()).map_err(encoder_error)
}

fn encoder_error(err: Error) -> String {
    alloc::format!("encoder: {err}")
}
