/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

use devkit_gfx::{Image, ImageFormat, Rect, Rotation, Size, Target};
use devkit_image::{JpegEncoder, JpegOptions, Subsampling, encode_buffer};
use idf::Error;

use super::{DecodeConfig, Decoder, Header, Sampling, Transform};
use crate::ppa::{Slot, Srm, Yuv, YuvRange, YuvStandard, ppa};

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
    let h = Header::parse(&[&jpeg(100, 36, ImageFormat::Rgb888, Subsampling::S420)]).unwrap();
    assert_eq!((h.width, h.height, h.padded_width, h.padded_height), (100, 36, 112, 48));
    assert_eq!((h.mcu, h.sampling), ((16, 16), Sampling::Yuv420));
    let h = Header::parse(&[&jpeg(64, 24, ImageFormat::Rgb888, Subsampling::S422)]).unwrap();
    assert_eq!((h.mcu, h.sampling, h.padded_height), ((16, 8), Sampling::Yuv422, 24));
    let h = Header::parse(&[&jpeg(40, 8, ImageFormat::L8, Subsampling::Default)]).unwrap();
    assert_eq!((h.sampling, h.padded_width), (Sampling::Gray, 40));
    assert_eq!(Header::parse(&[&[0xFF, 0xD8, 0xFF]]).err(), Some(Error::INVALID_ARG));
}

#[test]
fn a_whole_picture_lands_in_a_padded_target() {
    let data = jpeg(32, 16, ImageFormat::Rgb888, Subsampling::S420);
    let mut dec = Decoder::new(&config(ImageFormat::Bgr888)).unwrap();
    let pixels = aligned(32 * 16 * 3);
    let mut target = Target::new(&mut *pixels, ImageFormat::Bgr888, Size::new(32, 16), 32).unwrap();
    dec.decode(&[&data[..]], &mut target).unwrap().wait().unwrap();
    drop(target);
    assert!(pixels.iter().any(|&b| b != 0));

    let mut narrow = vec![0u8; 31 * 16 * 3];
    let mut target = Target::new(&mut narrow, ImageFormat::Bgr888, Size::new(31, 16), 31).unwrap();
    assert_eq!(dec.decode(&[&data[..]], &mut target).err(), Some(Error::INVALID_ARG));
}

#[test]
fn strips_cover_the_picture_in_order() {
    let data = jpeg(48, 72, ImageFormat::Rgb888, Subsampling::S420);
    let whole = aligned(48 * 80 * 2);
    let mut dec = Decoder::new(&config(ImageFormat::Rgb565)).unwrap();
    let mut target = Target::new(&mut *whole, ImageFormat::Rgb565, Size::new(48, 80), 48).unwrap();
    dec.decode(&[&data[..]], &mut target).unwrap().wait().unwrap();
    drop(target);

    let (a, b) = (aligned(48 * 2 * 32), aligned(48 * 2 * 32));
    let mut buffers = [a, b];
    let parts = [&data[..]];
    let mut strips = dec.strips(&parts, &mut buffers).unwrap();
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
    assert_eq!(dec.strips(&[&color[..]], &mut buffers).err(), Some(Error::NOT_SUPPORTED));
    let mut dec = Decoder::new(&config(ImageFormat::Rgb888)).unwrap();
    let mut tiny = [aligned(16 * 3 * 8)];
    assert_eq!(dec.strips(&[&color[..]], &mut tiny).err(), Some(Error::INVALID_SIZE));
}

/// What `render` lays down strip by strip is what one `Srm` of the whole
/// decoded picture lays down.
#[test]
fn a_render_matches_one_srm_of_the_whole_picture() {
    let data = jpeg(48, 72, ImageFormat::Rgb888, Subsampling::S420);
    let mut dec = Decoder::new(&config(ImageFormat::Rgb565)).unwrap();
    let whole = aligned(48 * 80 * 2);
    let mut target = Target::new(&mut *whole, ImageFormat::Rgb565, Size::new(48, 80), 48).unwrap();
    dec.decode(&[&data[..]], &mut target).unwrap().wait().unwrap();
    drop(target);
    let picture = Image::from_pixels(ImageFormat::Rgb565, 48, 72, 48, whole);

    let cases = [
        (Transform::new(), None),
        (Transform { rotation: Rotation::R90, at: (4, 2), ..Transform::new() }, None),
        (Transform { rotation: Rotation::R270, mirror_x: true, at: (-10, 5), ..Transform::new() }, Some(Rect::new(3, 4, 50, 30))),
        (Transform { rotation: Rotation::R180, src_rect: Some(Rect::new(5, 8, 40, 60)), at: (20, -7), ..Transform::new() }, Some(Rect::new(0, 0, 60, 40))),
    ];
    for (i, (t, area)) in cases.iter().enumerate() {
        let (mut a, mut b) = (vec![0u8; 80 * 80 * 2], vec![0u8; 80 * 80 * 2]);
        let mut target = Target::new(&mut a, ImageFormat::Rgb565, Size::new(80, 80), 80).unwrap();
        if let Some(area) = area {
            target = target.crop(*area).unwrap();
        }
        let mut buffers = [aligned(48 * 2 * 16), aligned(48 * 2 * 16)];
        dec.render(&[&data[..]], &mut buffers, t, &mut target).unwrap().wait().unwrap();
        drop(target);

        let mut target = Target::new(&mut b, ImageFormat::Rgb565, Size::new(80, 80), 80).unwrap();
        if let Some(area) = area {
            target = target.crop(*area).unwrap();
        }
        let mut op = Srm::new(picture);
        op.src_rect = t.src_rect.unwrap_or(op.src_rect);
        (op.at, op.rotation, op.mirror_x) = (t.at, t.rotation, t.mirror_x);
        ppa().unwrap().srm(&op, &mut target, &mut Slot::new()).unwrap().wait().unwrap();
        drop(target);
        assert!(a == b, "case {i}");
        assert!(a.iter().any(|&p| p != 0), "case {i}");
    }
}

#[test]
fn a_strip_edge_off_a_whole_row_is_refused() {
    let data = jpeg(48, 72, ImageFormat::Rgb888, Subsampling::S420);
    let mut dec = Decoder::new(&config(ImageFormat::Rgb565)).unwrap();
    let mut out = vec![0u8; 64 * 64 * 2];
    let mut target = Target::new(&mut out, ImageFormat::Rgb565, Size::new(64, 64), 64).unwrap();
    let mut buffers = [aligned(48 * 2 * 16)];
    let t = Transform { scale_x: 0.75, scale_y: 0.75, src_rect: Some(Rect::new(0, 1, 48, 40)), ..Transform::new() };
    assert_eq!(dec.render(&[&data[..]], &mut buffers, &t, &mut target).err(), Some(Error::INVALID_ARG));
    let t = Transform { scale_x: 0.75, scale_y: 0.75, src_rect: Some(Rect::new(0, 4, 48, 40)), ..Transform::new() };
    dec.render(&[&data[..]], &mut buffers, &t, &mut target).unwrap().wait().unwrap();
}

#[test]
fn pieces_decode_as_the_whole_file_does() {
    let data = jpeg(48, 72, ImageFormat::Rgb888, Subsampling::S420);
    let mut dec = Decoder::new(&config(ImageFormat::Rgb565)).unwrap();
    let whole = aligned(48 * 80 * 2);
    let mut target = Target::new(&mut *whole, ImageFormat::Rgb565, Size::new(48, 80), 48).unwrap();
    dec.decode(&[&data[..]], &mut target).unwrap().wait().unwrap();
    drop(target);
    let scan = Header::parse(&[&data]).unwrap().scan;
    for cut in [scan / 2, scan, scan + 1, data.len() - 3] {
        let (a, rest) = data.split_at(cut);
        let (b, c) = rest.split_at(rest.len() / 2);
        let pieces = aligned(48 * 80 * 2);
        let mut target = Target::new(&mut *pieces, ImageFormat::Rgb565, Size::new(48, 80), 48).unwrap();
        dec.decode(&[a, &[], b, c], &mut target).unwrap().wait().unwrap();
        drop(target);
        assert!(pieces[..] == whole[..], "cut at {cut}");
    }
}
