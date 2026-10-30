/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::format;
use alloc::string::{String, ToString};
use core::ffi::{c_int, c_void};
use core::mem;
use core::ptr;

use bsp_rs::display::{Display, PixelFormat, Rect};
use idf::sys;

use crate::ffi;

type Sink<'a> = &'a mut dyn FnMut(&[u8]) -> bool;

unsafe extern "C" fn write(user: *mut c_void, data: *const c_void, n: usize) -> c_int {
    let sink = unsafe { &mut *user.cast::<Sink>() };
    let bytes = unsafe { core::slice::from_raw_parts(data.cast::<u8>(), n) };
    if sink(bytes) { n as c_int } else { -1 }
}

/// Encodes what the panel shows, streaming the JPEG to `sink`.
pub(crate) fn encode(display: &Display, quality: u8, mut sink: Sink) -> Result<(), String> {
    let size = display.size();
    let format = match display.pixel_format() {
        PixelFormat::L8 => ffi::imgf_pixfmt_t_IMGF_PIX_GRAY8,
        PixelFormat::Rgb565 => ffi::imgf_pixfmt_t_IMGF_PIX_RGB565,
        PixelFormat::Rgb888 => ffi::imgf_pixfmt_t_IMGF_PIX_BGR888,
    };
    let mut opts: ffi::imgf_jpege_opts_t = unsafe { mem::zeroed() };
    opts.quality = c_int::from(quality);
    opts.alloc_caps = sys::MALLOC_CAP_8BIT;
    let mut err = 0;
    let encoder = unsafe { ffi::imgf_jpege_create(size.width, size.height, format, &opts, &mut err) };
    if encoder.is_null() {
        return Err(format!("encoder: {err}"));
    }
    let result = encode_rows(display, encoder, &mut sink);
    unsafe { ffi::imgf_encoder_destroy(encoder) };
    result
}

fn encode_rows(display: &Display, encoder: *mut ffi::imgf_encoder_t, sink: &mut Sink) -> Result<(), String> {
    let target = ffi::imgf_sink_t { write: Some(write), user: ptr::from_mut(sink).cast() };
    check(unsafe { ffi::imgf_encoder_bind_sink(encoder, target) })?;
    let size = display.size();
    let area = Rect { x: 0, y: 0, width: size.width, height: size.height };
    let mut rows = unsafe { display.readback(area) }.map_err(|e| e.to_string())?;
    while let Some(chunk) = rows.next().map_err(|e| e.to_string())? {
        for r in 0..usize::from(chunk.height) {
            let row = chunk.data[r * chunk.stride..].as_ptr();
            if unsafe { ffi::imgf_encoder_push_row(encoder, row) } != 1 {
                return Err(format!("row: {}", unsafe { ffi::imgf_encoder_last_error(encoder) }));
            }
        }
    }
    check(unsafe { ffi::imgf_encoder_finish(encoder, ptr::null_mut()) })
}

fn check(err: ffi::imgf_err_t) -> Result<(), String> {
    if err == ffi::IMGF_OK as ffi::imgf_err_t { Ok(()) } else { Err(format!("encoder: {err}")) }
}
