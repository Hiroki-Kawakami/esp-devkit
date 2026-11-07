/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The C jpeg_decode_enhanced, the reference media_engine's decoder is compared against.
 */

use alloc::boxed::Box;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use devkit_gfx::ImageFormat;
use idf::{Error, Result};

use crate::ffi as sys;

pub struct Decoder {
    handle: Box<AtomicPtr<sys::jpeg_enh_strip_decoder_s>>,
}

unsafe extern "C" fn on_strip(evt: *const sys::jpeg_enh_strip_event_t, ctx: *mut c_void) -> bool {
    unsafe {
        let handle = (*ctx.cast::<AtomicPtr<sys::jpeg_enh_strip_decoder_s>>()).load(Ordering::Relaxed);
        sys::jpeg_enh_strip_decoder_release_strip(handle, (*evt).strip_idx);
    }
    false
}

impl Decoder {
    /// Strips are released as soon as they land, so they cost only the decode.
    pub fn new(format: ImageFormat, strips: [*mut u8; 2], strip_size: usize) -> Result<Self> {
        let (output_format, rgb_order) = match format {
            ImageFormat::Rgb888 => (sys::jpeg_dec_output_format_t_JPEG_DECODE_OUT_FORMAT_RGB888, sys::jpeg_dec_rgb_element_order_t_JPEG_DEC_RGB_ELEMENT_ORDER_RGB),
            ImageFormat::Bgr888 => (sys::jpeg_dec_output_format_t_JPEG_DECODE_OUT_FORMAT_RGB888, sys::jpeg_dec_rgb_element_order_t_JPEG_DEC_RGB_ELEMENT_ORDER_BGR),
            ImageFormat::Rgb565 => (sys::jpeg_dec_output_format_t_JPEG_DECODE_OUT_FORMAT_RGB565, sys::jpeg_dec_rgb_element_order_t_JPEG_DEC_RGB_ELEMENT_ORDER_BGR),
            ImageFormat::L8 => (sys::jpeg_dec_output_format_t_JPEG_DECODE_OUT_FORMAT_GRAY, sys::jpeg_dec_rgb_element_order_t_JPEG_DEC_RGB_ELEMENT_ORDER_BGR),
            _ => return Err(Error::NOT_SUPPORTED),
        };
        let handle = Box::new(AtomicPtr::new(ptr::null_mut()));
        let mut cfg: sys::jpeg_enh_strip_decoder_cfg_t = unsafe { core::mem::zeroed() };
        cfg.decode.output_format = output_format;
        cfg.decode.rgb_order = rgb_order;
        cfg.decode.conv_std = sys::jpeg_yuv_rgb_conv_std_t_JPEG_YUV_RGB_CONV_STD_BT601;
        cfg.decode.yuv_full_range = true;
        cfg.strip_bufs = strips.map(|p| p.cast());
        cfg.strip_buf_size = strip_size;
        cfg.on_strip_done = Some(on_strip);
        cfg.user_ctx = ptr::from_ref(&*handle).cast_mut().cast();
        let mut raw = ptr::null_mut();
        Error::check(unsafe { sys::jpeg_enh_strip_decoder_new(&cfg, &mut raw) })?;
        handle.store(raw, Ordering::Relaxed);
        Ok(Self { handle })
    }

    fn raw(&self) -> sys::jpeg_enh_strip_decoder_handle_t {
        self.handle.load(Ordering::Relaxed)
    }

    pub fn decode(&mut self, data: &[u8], out: &mut [u8]) -> Result<()> {
        let mut info = unsafe { core::mem::zeroed() };
        Error::check(unsafe {
            sys::jpeg_enh_decoder_process(self.raw(), data.as_ptr(), data.len() as u32, out.as_mut_ptr().cast(), out.len(), &mut info)
        })
    }

    pub fn strips(&mut self, data: &[u8]) -> Result<u32> {
        let mut info: sys::jpeg_enh_frame_info_t = unsafe { core::mem::zeroed() };
        Error::check(unsafe { sys::jpeg_enh_strip_decoder_process(self.raw(), data.as_ptr(), data.len() as u32, &mut info) })?;
        Ok(info.strip_count)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { sys::jpeg_enh_strip_decoder_del(self.raw()) };
    }
}

pub struct Pipeline {
    handle: sys::jpeg_ppa_pipeline_handle_t,
}

/// What `jpeg_ppa_transform_t` says, in plain numbers.
pub struct Case {
    pub quarter_turns: u32,
    pub scale: f32,
    pub mirror_x: bool,
    pub crop: Option<(u32, u32, u32, u32)>,
    pub offset: (u32, u32),
    pub clip: Option<(u32, u32, u32, u32)>,
}

impl Pipeline {
    pub fn new(strips: [*mut u8; 2], strip_size: usize) -> Result<Self> {
        let mut cfg: sys::jpeg_ppa_pipeline_cfg_t = unsafe { core::mem::zeroed() };
        cfg.strip_bufs = strips.map(|p| p.cast());
        cfg.strip_buf_size = strip_size;
        cfg.strip_color_mode = sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565;
        cfg.rgb_order = sys::jpeg_dec_rgb_element_order_t_JPEG_DEC_RGB_ELEMENT_ORDER_BGR;
        cfg.conv_std = sys::jpeg_yuv_rgb_conv_std_t_JPEG_YUV_RGB_CONV_STD_BT601;
        cfg.yuv_full_range = true;
        let mut handle = ptr::null_mut();
        Error::check(unsafe { sys::jpeg_ppa_pipeline_new(&cfg, &mut handle) })?;
        Ok(Self { handle })
    }

    /// Into an RGB565 picture of `size` at `out`.
    pub fn process(&mut self, data: &[u8], out: &mut [u8], size: (u32, u32), case: &Case) -> Result<()> {
        let rect = |r: Option<(u32, u32, u32, u32)>| {
            let (x, y, w, h) = r.unwrap_or_default();
            sys::jpeg_ppa_rect_t { x, y, w, h }
        };
        let mut output: sys::jpeg_ppa_output_t = unsafe { core::mem::zeroed() };
        output.buffer = out.as_mut_ptr().cast();
        output.buffer_size = out.len();
        (output.pic_w, output.pic_h) = size;
        output.color_mode = sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565;
        let mut t: sys::jpeg_ppa_transform_t = unsafe { core::mem::zeroed() };
        t.rotation = case.quarter_turns;
        t.scale_x = case.scale;
        t.scale_y = case.scale;
        t.mirror_x = case.mirror_x;
        t.in_crop = rect(case.crop);
        (t.out_offset_x, t.out_offset_y) = case.offset;
        t.out_clip = rect(case.clip);
        Error::check(unsafe { sys::jpeg_ppa_pipeline_process(self.handle, data.as_ptr().cast(), data.len(), &output, &t, ptr::null_mut()) })
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        unsafe { sys::jpeg_ppa_pipeline_del(self.handle) };
    }
}
