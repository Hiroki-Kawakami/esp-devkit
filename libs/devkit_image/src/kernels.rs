/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ptr;

use devkit::A16;

use crate::ImageFormat;

mod p4 {
    unsafe extern "C" {
        pub fn imgf_k_prepare();
        pub fn imgf_k_ycc_rgb_h1(dst: *mut u8, y: *const u8, cb: *const u8, cr: *const u8, n16: i32);
        pub fn imgf_k_ycc_rgb_h0(dst: *mut u8, y: *const u8, cb: *const u8, cr: *const u8, n16: i32);
        pub fn imgf_k_ycc_bgr_h1(dst: *mut u8, y: *const u8, cb: *const u8, cr: *const u8, n16: i32);
        pub fn imgf_k_ycc_bgr_h0(dst: *mut u8, y: *const u8, cb: *const u8, cr: *const u8, n16: i32);
        pub fn imgf_k_ycc_565_h1(dst: *mut u8, y: *const u8, cb: *const u8, cr: *const u8, n16: i32);
        pub fn imgf_k_ycc_565_h0(dst: *mut u8, y: *const u8, cb: *const u8, cr: *const u8, n16: i32);
        pub fn imgf_k_idct(dst: *mut u8, stride: i32, dqc: *const i16, rowmask: i32, tab: *const i16);
        pub fn imgf_k_pack_bytes(dst: *mut u8, src: *const u16, n16: i32);
        pub fn imgf_k_pack_rgb565(dst: *mut u8, src: *const u16, n8: i32);
        pub static imgf_idct_tab8: i16;
        pub static imgf_idct_tab4: i16;
        pub static imgf_idct_tab2: i16;
    }
}

pub(crate) enum IdctSize {
    Full,
    Half,
    Quarter,
}

/// `y`, `cb` and `cr` must stay valid 16 bytes past their last row: the
/// kernel reads 16 chroma bytes per group. `format` is `Rgb888`, `Bgr888` or
/// `Rgb565`.
pub(crate) unsafe fn ycc_rgb(dst: *mut u8, y: *const u8, cb: *const u8, cr: *const u8, n16: usize, half: bool, format: ImageFormat) {
    let kernel = match (format, half) {
        (ImageFormat::Rgb888, true) => p4::imgf_k_ycc_rgb_h1,
        (ImageFormat::Rgb888, false) => p4::imgf_k_ycc_rgb_h0,
        (ImageFormat::Bgr888, true) => p4::imgf_k_ycc_bgr_h1,
        (ImageFormat::Bgr888, false) => p4::imgf_k_ycc_bgr_h0,
        (ImageFormat::Rgb565, true) => p4::imgf_k_ycc_565_h1,
        (ImageFormat::Rgb565, false) => p4::imgf_k_ycc_565_h0,
        _ => unreachable!(),
    };
    unsafe {
        p4::imgf_k_prepare();
        kernel(dst, y, cb, cr, n16 as i32);
    }
}

/// Writes 8x8 whatever the size, with the answer in the top-left.
pub(crate) unsafe fn idct(dst: *mut u8, stride: usize, dqc: &A16<[i16; 64]>, rowmask: u32, size: IdctSize) {
    let tab = match size {
        IdctSize::Full => ptr::addr_of!(p4::imgf_idct_tab8),
        IdctSize::Half => ptr::addr_of!(p4::imgf_idct_tab4),
        IdctSize::Quarter => ptr::addr_of!(p4::imgf_idct_tab2),
    };
    unsafe {
        p4::imgf_k_prepare();
        p4::imgf_k_idct(dst, stride as i32, dqc.as_ptr(), rowmask as i32, tab);
    }
}

pub(crate) fn pack_bytes(dst: &mut [u8], src: &[u16], n16: usize) {
    assert!(dst.len() >= n16 * 16 && src.len() >= n16 * 16);
    unsafe {
        p4::imgf_k_prepare();
        p4::imgf_k_pack_bytes(dst.as_mut_ptr(), src.as_ptr(), n16 as i32);
    }
}

pub(crate) fn pack_rgb565(dst: &mut [u8], src: &[u16], n8: usize) {
    assert!(dst.len() >= n8 * 16 && src.len() >= n8 * 24);
    unsafe {
        p4::imgf_k_prepare();
        p4::imgf_k_pack_rgb565(dst.as_mut_ptr(), src.as_ptr(), n8 as i32);
    }
}
