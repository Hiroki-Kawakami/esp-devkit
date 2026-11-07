/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

use devkit_gfx::{ImageFormat, Size, Target};
use devkit_image::{JpegEncoder, JpegOptions, Subsampling, encode_buffer};
use idf::Error;

use super::{DecodeConfig, Decoder, Header, Sampling};
use crate::ppa::{Yuv, YuvRange, YuvStandard};

fn jpeg(w: u16, h: u16, format: ImageFormat, subsampling: Subsampling) -> Vec<u8> {
    let channels = if format == ImageFormat::L8 { 1 } else { 3 };
    let src: Vec<u8> = (0..usize::from(w) * usize::from(h) * channels).map(|i| (i * 7 % 251) as u8).collect();
    let mut out = Vec::new();
    let sink = |data: &[u8]| {
        out.extend_from_slice(data);
        Ok(())
    };
    let options = JpegOptions { quality: 90, subsampling, ..Default::default() };
    let encoder = JpegEncoder::new(w, h, format, &options, sink).unwrap();
    let _ = encode_buffer(encoder, &src, 0).unwrap();
    out
}

fn config(format: ImageFormat) -> DecodeConfig {
    DecodeConfig { format, yuv: Yuv { range: YuvRange::Full, standard: YuvStandard::Bt601 } }
}

/// 64-byte aligned, as the 2D-DMA wants.
fn aligned(len: usize) -> &'static mut [u8] {
    let layout = Layout::from_size_align(len.next_multiple_of(64), 64).unwrap();
    unsafe { core::slice::from_raw_parts_mut(alloc::alloc::alloc_zeroed(layout), layout.size()) }
}

#[test]
fn the_header_rounds_the_size_up_to_whole_mcus() {
    let h = Header::parse(&jpeg(100, 36, ImageFormat::Rgb888, Subsampling::S420)).unwrap();
    assert_eq!((h.width, h.height, h.padded_width, h.padded_height), (100, 36, 112, 48));
    assert_eq!((h.mcu, h.sampling), ((16, 16), Sampling::Yuv420));
    let h = Header::parse(&jpeg(64, 24, ImageFormat::Rgb888, Subsampling::S422)).unwrap();
    assert_eq!((h.mcu, h.sampling, h.padded_height), ((16, 8), Sampling::Yuv422, 24));
    let h = Header::parse(&jpeg(40, 8, ImageFormat::L8, Subsampling::Default)).unwrap();
    assert_eq!((h.sampling, h.padded_width), (Sampling::Gray, 40));
    assert_eq!(Header::parse(&[0xFF, 0xD8, 0xFF]).err(), Some(Error::INVALID_ARG));
}

#[test]
fn a_whole_picture_lands_in_a_padded_target() {
    let data = jpeg(32, 16, ImageFormat::Rgb888, Subsampling::S420);
    let mut dec = Decoder::new(&config(ImageFormat::Bgr888)).unwrap();
    let pixels = aligned(32 * 16 * 3);
    let mut target = Target::new(&mut *pixels, ImageFormat::Bgr888, Size::new(32, 16), 32).unwrap();
    dec.decode(&data, &mut target).unwrap().wait().unwrap();
    drop(target);
    assert!(pixels.iter().any(|&b| b != 0));

    let mut narrow = vec![0u8; 31 * 16 * 3];
    let mut target = Target::new(&mut narrow, ImageFormat::Bgr888, Size::new(31, 16), 31).unwrap();
    assert_eq!(dec.decode(&data, &mut target).err(), Some(Error::INVALID_ARG));
}

#[test]
fn strips_cover_the_picture_in_order() {
    let data = jpeg(48, 72, ImageFormat::Rgb888, Subsampling::S420);
    let whole = aligned(48 * 80 * 2);
    let mut dec = Decoder::new(&config(ImageFormat::Rgb565)).unwrap();
    let mut target = Target::new(&mut *whole, ImageFormat::Rgb565, Size::new(48, 80), 48).unwrap();
    dec.decode(&data, &mut target).unwrap().wait().unwrap();
    drop(target);

    let (a, b) = (aligned(48 * 2 * 32), aligned(48 * 2 * 32));
    let mut buffers = [a, b];
    let mut strips = dec.strips(&data, &mut buffers).unwrap();
    assert_eq!(strips.strip_rows(), 32);
    let mut seen = Vec::new();
    while let Some(strip) = strips.next_blocking().unwrap() {
        let at = usize::from(strip.y()) * strip.stride_bytes();
        let len = usize::from(strip.padded_rows()) * strip.stride_bytes();
        assert_eq!(&strip.pixels()[..len], &whole[at..at + len]);
        seen.push((strip.index(), strip.y(), strip.rows(), strip.padded_rows()));
    }
    assert_eq!(seen, [(0, 0, 32, 32), (1, 32, 32, 32), (2, 64, 8, 16)]);
}

#[test]
fn what_the_codec_cannot_do_is_refused() {
    assert_eq!(Decoder::new(&config(ImageFormat::Yuv420)).err(), Some(Error::NOT_SUPPORTED));
    let color = jpeg(16, 16, ImageFormat::Rgb888, Subsampling::S444);
    let mut dec = Decoder::new(&config(ImageFormat::L8)).unwrap();
    let mut buffers = [aligned(16 * 16)];
    assert_eq!(dec.strips(&color, &mut buffers).err(), Some(Error::NOT_SUPPORTED));
    let mut dec = Decoder::new(&config(ImageFormat::Rgb888)).unwrap();
    let mut tiny = [aligned(16 * 3 * 8)];
    assert_eq!(dec.strips(&color, &mut tiny).err(), Some(Error::INVALID_SIZE));
}
