/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::{String, ToString};

use bsp_rs::display::{Display, Rect};
use devkit_image::{Encoder, Error, JpegEncoder, JpegOptions};
use idf::heap::Caps;

type Sink<'a> = &'a mut dyn FnMut(&[u8]) -> bool;

/// Encodes what the panel shows, streaming the JPEG to `sink`.
pub(crate) fn encode(display: &Display, quality: u8, sink: Sink) -> Result<(), String> {
    let size = display.size();
    let format = display.pixel_format();
    let options = JpegOptions { quality, caps: Some(Caps::ACCESS_8BIT), ..Default::default() };
    let write = |data: &[u8]| if sink(data) { Ok(()) } else { Err(Error::Io) };
    let mut encoder = JpegEncoder::new(size.width, size.height, format, &options, write).map_err(encoder_error)?;

    let area = Rect::new(0, 0, size.width as i16, size.height as i16);
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
