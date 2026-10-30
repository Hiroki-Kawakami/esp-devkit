/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::ffi::CString;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use idf::fs::File;

use crate::pipeline::{self, PipelineOptions, Ring};
use crate::*;

fn fixture(name: &str) -> Vec<u8> {
    let dir = std::env::var("DEVKIT_TEST_FIXTURES").expect("run through test/run.sh");
    std::fs::read(format!("{dir}/{name}.bin")).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn fixture_size(name: &str) -> (usize, usize) {
    let dir = std::env::var("DEVKIT_TEST_FIXTURES").unwrap();
    let text = std::fs::read_to_string(format!("{dir}/{name}.size")).unwrap();
    let mut it = text.split_whitespace().map(|v| v.parse().unwrap());
    (it.next().unwrap(), it.next().unwrap())
}

fn near(a: u8, b: i32, tol: i32) -> bool {
    (i32::from(a) - b).abs() <= tol
}

struct OneByte<'a>(&'a [u8]);

impl Stream for OneByte<'_> {
    fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        let Some((&b, rest)) = self.0.split_first() else {
            return Ok(0);
        };
        dst[0] = b;
        self.0 = rest;
        Ok(1)
    }
}

fn opts() -> DecodeOptions {
    DecodeOptions::default()
}

fn jpeg(bytes: &[u8]) -> Result<JpegDecoder<'_>> {
    JpegDecoder::open(SliceStream::new(bytes), &opts())
}

fn png(bytes: &[u8]) -> Result<PngDecoder<'_>> {
    PngDecoder::open(SliceStream::new(bytes), &opts())
}

fn drain<F: FnMut(&mut [u8]) -> Result<bool>>(mut next: F, row: &mut [u8]) -> Result<usize> {
    let mut rows = 0;
    while next(row)? {
        rows += 1;
    }
    Ok(rows)
}

#[test]
fn error_strings() {
    assert_eq!(Error::Truncated.to_string(), "Truncated");
    assert_eq!(Error::Unsupported.to_string(), "UnsupportedFormat");
    assert_eq!(Error::TooLarge.to_string(), "TooLarge");
}

#[test]
fn slice_stream() {
    let src = [1, 2, 3, 4, 5];
    let mut s = SliceStream::new(&src);
    let mut buf = [0; 3];
    assert_eq!(s.read(&mut buf), Ok(3));
    assert_eq!(buf, [1, 2, 3]);
    assert_eq!(s.read(&mut buf), Ok(2));
    assert_eq!(buf[..2], [4, 5]);
    assert_eq!(s.read(&mut buf), Ok(0));
}

fn temp_file(tag: &str, parts: &[&[u8]]) -> (std::path::PathBuf, File) {
    let path = std::env::temp_dir().join(format!("devkit_image_{}_{tag}", std::process::id()));
    std::fs::write(&path, parts.concat()).unwrap();
    let c = CString::new(path.to_str().unwrap()).unwrap();
    let file = File::open(&c).unwrap();
    (path, file)
}

#[test]
fn file_stream() {
    let (path, file) = temp_file("whole", &[&[0xDE, 0xAD, 0xBE, 0xEF]]);
    let mut s = FileStream::new(&file, 0, None);
    let mut got = [0; 4];
    assert_eq!(s.read(&mut got), Ok(4));
    assert_eq!(got, [0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(s.read(&mut got), Ok(0));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn file_subrange_jpeg() {
    let body = fixture("kJpgGrayBig");
    let pad = [0; 100];
    let (path, file) = temp_file("subrange", &[&pad, &body, &pad]);
    let d = JpegDecoder::open(FileStream::new(&file, 100, Some(body.len())), &opts()).unwrap();
    assert_eq!((d.info().width, d.info().height), (64, 64));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn stream_one_byte_at_a_time() {
    let bytes = fixture("kPngRgb2x2");
    let d = PngDecoder::open(OneByte(&bytes), &opts()).unwrap();
    assert_eq!((d.info().width, d.info().height), (2, 2));
}

#[test]
fn sniffs_every_format() {
    let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    assert_eq!(sniff(&png), Some(Format::Png));
    assert_eq!(sniff(&[0xFF, 0xD8, 0xFF]), Some(Format::Jpeg));
    assert_eq!(sniff(b"BM"), Some(Format::Bmp));
    assert_eq!(sniff(b"GIF89a"), Some(Format::Gif));
    assert_eq!(sniff(b"GIF87a"), Some(Format::Gif));
    assert_eq!(sniff(b"II*\0"), Some(Format::Tiff));
    assert_eq!(sniff(b"MM\0*"), Some(Format::Tiff));
    assert_eq!(sniff(b"RIFF\0\0\0\0WEBP"), Some(Format::Webp));
    assert_eq!(sniff(b"RIFF\0\0\0\0WAVE"), None);
    assert_eq!(sniff(&[0, 1, 2, 3, 4, 5, 6, 7]), None);
    assert_eq!(sniff(&png[..4]), None);
}

#[test]
fn open_picks_the_decoder() {
    for (name, expected) in [
        ("kJpgRgbRed", Format::Jpeg),
        ("kPngRgb2x2", Format::Png),
        ("kBmp24", Format::Bmp),
        ("kGif", Format::Gif),
        ("kTiffRgb", Format::Tiff),
        ("kWebpLossy", Format::Webp),
    ] {
        let bytes = fixture(name);
        let d = open(SliceStream::new(&bytes), &opts()).unwrap();
        let format = match d {
            Decoder::Jpeg(_) => Format::Jpeg,
            Decoder::Png(_) => Format::Png,
            Decoder::Bmp(_) => Format::Bmp,
            Decoder::Gif(_) => Format::Gif,
            Decoder::Tiff(_) => Format::Tiff,
            Decoder::Webp(_) => Format::Webp,
        };
        assert_eq!(format, expected);
    }
    assert!(matches!(open(SliceStream::new(&[0; 16]), &opts()), Err(Error::Unsupported)));
}

#[test]
fn open_without_seek_keeps_the_sniffed_bytes() {
    let bytes = fixture("kPngGray2x2");
    let mut d = open(OneByte(&bytes), &opts()).unwrap();
    let mut row = [0; 2];
    assert_eq!(d.next_row(&mut row), Ok(true));
    assert_eq!(row, [0, 255]);
}

#[test]
fn jpeg_gray_flat() {
    let bytes = fixture("kJpgGrayFlat");
    let mut d = jpeg(&bytes).unwrap();
    assert_eq!(d.info(), ImageInfo { width: 16, height: 16, format: PixelFormat::Gray8 });
    let mut row = [0; 16];
    let mut rows = 0;
    while d.next_row(&mut row).unwrap() {
        assert!(row.iter().all(|&v| near(v, 128, 4)));
        rows += 1;
    }
    assert_eq!(rows, 16);
}

#[test]
fn jpeg_gray_split() {
    let bytes = fixture("kJpgGraySplit");
    let mut d = jpeg(&bytes).unwrap();
    let mut row = [0; 16];
    assert!(d.next_row(&mut row).unwrap());
    assert!(near(row[2], 40, 25));
    assert!(near(row[13], 220, 25));
    assert!(i32::from(row[13]) > i32::from(row[2]) + 100);
}

fn check_red(row: &[u8]) {
    assert!(near(row[0], 220, 20));
    assert!(near(row[1], 30, 25));
    assert!(near(row[2], 30, 25));
}

#[test]
fn jpeg_rgb_red() {
    let bytes = fixture("kJpgRgbRed");
    let mut d = jpeg(&bytes).unwrap();
    assert_eq!(d.info().format, PixelFormat::Rgb888);
    let mut row = [0; 16 * 3];
    assert!(d.next_row(&mut row).unwrap());
    check_red(&row);
}

#[test]
fn jpeg_decode_time_downscale() {
    let bytes = fixture("kJpgGrayBig");
    let o = DecodeOptions { target_width: 16, target_height: 16, ..opts() };
    let mut d = JpegDecoder::open(SliceStream::new(&bytes), &o).unwrap();
    let w = d.info().width;
    assert!((16..64).contains(&w));
    let mut row = [0; 64];
    assert!(d.next_row(&mut row).unwrap());
    assert!(near(row[0], 100, 6));
}

#[test]
fn jpeg_truncated_header() {
    assert!(matches!(jpeg(&[0xFF, 0xD8]), Err(Error::Truncated)));
}

#[test]
fn jpeg_unsupported_sof() {
    assert!(matches!(jpeg(&[0xFF, 0xD8, 0xFF, 0xC9, 0x00, 0x0B]), Err(Error::Unsupported)));
}

#[test]
fn jpeg_progressive_gray() {
    let bytes = fixture("kJpgProgGraySplit");
    let mut d = jpeg(&bytes).unwrap();
    assert_eq!(d.info(), ImageInfo { width: 16, height: 16, format: PixelFormat::Gray8 });
    let mut row = [0; 16];
    let mut rows = 0;
    while d.next_row(&mut row).unwrap() {
        assert!(near(row[2], 40, 25));
        assert!(near(row[13], 220, 25));
        rows += 1;
    }
    assert_eq!(rows, 16);
}

#[test]
fn jpeg_progressive_rgb() {
    let bytes = fixture("kJpgProgRgbRed");
    let mut d = jpeg(&bytes).unwrap();
    assert_eq!(d.info().format, PixelFormat::Rgb888);
    let mut row = [0; 16 * 3];
    assert!(d.next_row(&mut row).unwrap());
    check_red(&row);
}

// libjpeg quantizes the same coefficients either way, so the two encodings of one
// picture must come out of the decoder identical.
fn same_rows(baseline: &str, progressive: &str) {
    let (b, p) = (fixture(baseline), fixture(progressive));
    let mut b = jpeg(&b).unwrap();
    let mut p = jpeg(&p).unwrap();
    assert_eq!((p.info().width, p.info().height), (37, 23));
    assert_eq!(b.info(), p.info());
    let (mut row_b, mut row_p) = ([0; 37 * 3], [0; 37 * 3]);
    let mut rows = 0;
    while b.next_row(&mut row_b).unwrap() {
        assert!(p.next_row(&mut row_p).unwrap());
        assert_eq!(row_b, row_p);
        rows += 1;
    }
    assert_eq!(rows, 23);
    assert_eq!(p.next_row(&mut row_p), Ok(false));
}

#[test]
fn jpeg_progressive_matches_baseline() {
    same_rows("kJpgGrad", "kJpgProgGrad");
}

#[test]
fn jpeg_progressive_restart() {
    same_rows("kJpgGrad", "kJpgProgGradRst");
}

#[test]
fn jpeg_progressive_downscale() {
    let bytes = fixture("kJpgProgGrad");
    let o = DecodeOptions { target_width: 8, target_height: 8, ..opts() };
    let mut d = JpegDecoder::open(SliceStream::new(&bytes), &o).unwrap();
    let info = d.info();
    assert!(info.width < 37 && info.width >= 8);
    let mut row = [0; 37 * 3];
    assert_eq!(drain(|r| d.next_row(r), &mut row), Ok(usize::from(info.height)));
}

#[test]
fn jpeg_progressive_truncated_keeps_the_scans_that_arrived() {
    let bytes = fixture("kJpgProgGrad");
    let mut d = jpeg(&bytes[..bytes.len() / 2]).unwrap();
    let mut row = [0; 37 * 3];
    assert_eq!(drain(|r| d.next_row(r), &mut row), Ok(23));
}

#[test]
fn png_gray() {
    let bytes = fixture("kPngGray2x2");
    let mut d = png(&bytes).unwrap();
    assert_eq!(d.info(), ImageInfo { width: 2, height: 2, format: PixelFormat::Gray8 });
    let mut row = [0; 2];
    assert_eq!(d.next_row(&mut row), Ok(true));
    assert_eq!(row, [0, 255]);
    assert_eq!(d.next_row(&mut row), Ok(true));
    assert_eq!(row, [128, 64]);
    assert_eq!(d.next_row(&mut row), Ok(false));
}

#[test]
fn png_rgb() {
    let bytes = fixture("kPngRgb2x2");
    let mut d = png(&bytes).unwrap();
    assert_eq!(d.info().format, PixelFormat::Rgb888);
    let mut row = [0; 6];
    assert!(d.next_row(&mut row).unwrap());
    assert_eq!(row, [255, 0, 0, 0, 255, 0]);
    assert!(d.next_row(&mut row).unwrap());
    assert_eq!(row, [0, 0, 255, 255, 255, 255]);
}

#[test]
fn png_palette() {
    let bytes = fixture("kPngPalette2x2");
    let mut d = png(&bytes).unwrap();
    assert_eq!(d.info().format, PixelFormat::Rgb888);
    let mut row = [0; 6];
    assert!(d.next_row(&mut row).unwrap());
    assert_eq!(row, [255, 0, 0, 0, 255, 0]);
    assert!(d.next_row(&mut row).unwrap());
    assert_eq!(row[..3], [0, 0, 255]);
}

#[test]
fn png_rgba_composite() {
    let bytes = fixture("kPngRgba2x2");
    let mut d = png(&bytes).unwrap();
    let mut row = [0; 6];
    assert!(d.next_row(&mut row).unwrap());
    assert_eq!(row, [227, 177, 152, 10, 20, 30]);
    assert!(d.next_row(&mut row).unwrap());
    assert_eq!(row[..3], [255, 255, 255]);
}

#[test]
fn png_gradient_inflate() {
    let bytes = fixture("kPngGrad16");
    let mut d = png(&bytes).unwrap();
    assert_eq!((d.info().width, d.info().height), (16, 16));
    let mut row = [0; 16];
    let mut y = 0;
    while d.next_row(&mut row).unwrap() {
        for (x, &v) in row.iter().enumerate() {
            assert_eq!(usize::from(v), (x * 16 + y * 4) & 0xFF);
        }
        y += 1;
    }
    assert_eq!(y, 16);
}

#[test]
fn png_truncated_idat() {
    let bytes = fixture("kPngGrad16");
    let cut = &bytes[..bytes.len() / 2];
    match png(cut) {
        Ok(mut d) => {
            let mut row = [0; 16];
            assert_eq!(drain(|r| d.next_row(r), &mut row), Err(Error::Truncated));
        }
        Err(e) => assert_eq!(e, Error::Truncated),
    }
}

#[test]
fn png_signature_only() {
    let sig = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    assert!(matches!(png(&sig), Err(Error::Truncated)));
}

fn like_pillow(format: Format, name: &str, pixel: PixelFormat, tol: i32) {
    let bytes = fixture(name);
    let pixels = fixture(&format!("{name}Pixels"));
    let (w, h) = fixture_size(name);
    let mut d = Decoder::open(format, SliceStream::new(&bytes), &opts()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(d.info(), ImageInfo { width: w as u16, height: h as u16, format: pixel }, "{name}");
    let stride = w * pixel.bytes_per_pixel();
    let mut row = vec![0; stride.max(1)];
    for y in 0..h {
        assert_eq!(d.next_row(&mut row), Ok(true), "{name} row {y}");
        for i in 0..stride {
            assert!(near(row[i], i32::from(pixels[y * stride + i]), tol), "{name} row {y} byte {i}");
        }
    }
    assert_eq!(d.next_row(&mut row), Ok(false), "{name}");
}

#[test]
fn bmp_like_pillow() {
    use PixelFormat::Rgb888;
    for (name, tol) in [
        ("kBmp1", 0), ("kBmp4", 0), ("kBmp8", 0), ("kBmp24", 0), ("kBmp24Os2", 0), ("kBmp16", 1),
        ("kBmp565", 1), ("kBmp32", 0), ("kBmp32V5", 0), ("kBmpRle8", 0), ("kBmpRle4", 0),
    ] {
        like_pillow(Format::Bmp, name, Rgb888, tol);
    }
}

#[test]
fn gif_like_pillow() {
    for name in ["kGif", "kGifInterlaced", "kGifTransparent", "kGifAnimated", "kGifOffset"] {
        like_pillow(Format::Gif, name, PixelFormat::Rgb888, 0);
    }
}

#[test]
fn gif_truncated() {
    let bytes = fixture("kGif");
    let (w, _) = fixture_size("kGif");
    let mut d = GifDecoder::open(SliceStream::new(&bytes[..bytes.len() - 40]), &opts()).unwrap();
    let mut row = vec![0; w * 3];
    assert_eq!(drain(|r| d.next_row(r), &mut row), Err(Error::Truncated));
}

#[test]
fn tiff_like_pillow() {
    use PixelFormat::{Gray8, Rgb888};
    for (name, format, tol) in [
        ("kTiffGray", Gray8, 0), ("kTiffRgb", Rgb888, 0), ("kTiffPalette", Rgb888, 0),
        ("kTiffRgba", Rgb888, 1), ("kTiffGrayAlpha", Gray8, 1), ("kTiffBilevel", Gray8, 0),
        ("kTiffTiled", Rgb888, 0), ("kTiffTiledBig", Rgb888, 0), ("kTiffWhite4", Gray8, 0),
        ("kTiffRgb16", Rgb888, 0), ("kTiffAssociated", Rgb888, 0),
    ] {
        like_pillow(Format::Tiff, name, format, tol);
    }
}

#[test]
fn tiff_without_seek() {
    let bytes = fixture("kTiffRgb");
    assert!(matches!(TiffDecoder::open(OneByte(&bytes), &opts()), Err(Error::Unsupported)));
}

#[test]
fn tiff_truncated() {
    let bytes = fixture("kTiffTiled");
    assert!(matches!(TiffDecoder::open(SliceStream::new(&bytes[..200]), &opts()), Err(Error::Truncated)));
}

#[test]
fn webp_like_pillow() {
    for (name, tol) in [
        ("kWebpLossy", 0), ("kWebpLossySimple", 0), ("kWebpLossyNoFilter", 0), ("kWebpLossyQ100", 0),
        ("kWebpLossyParts", 0), ("kWebpLossless", 0), ("kWebpLosslessZ9", 0), ("kWebpLosslessAlpha", 1),
        ("kWebpPalette2", 0), ("kWebpPalette4", 0), ("kWebpPalette16", 0), ("kWebpPalette200", 0),
        ("kWebpAlpha", 1), ("kWebpAlphaHorizontal", 1), ("kWebpAlphaVertical", 1), ("kWebpAlphaGradient", 1),
        ("kWebpAnimated", 1), ("kWebpAnimOffset", 0),
    ] {
        like_pillow(Format::Webp, name, PixelFormat::Rgb888, tol);
    }
}

#[test]
fn webp_far_copies() {
    let dir = std::env::var("DEVKIT_TEST_FIXTURES").unwrap();
    let expected: u32 = std::fs::read_to_string(format!("{dir}/kWebpFar.hash")).unwrap().trim().parse().unwrap();
    let bytes = fixture("kWebpFar");
    let (w, h) = fixture_size("kWebpFar");
    let mut d = WebpDecoder::open(SliceStream::new(&bytes), &opts()).unwrap();
    assert_eq!((usize::from(d.info().width), usize::from(d.info().height)), (w, h));
    let mut row = vec![0; w * 3];
    let mut hash = 0x811c9dc5u32;
    for _ in 0..h {
        assert_eq!(d.next_row(&mut row), Ok(true));
        hash = row.iter().fold(hash, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x01000193));
    }
    assert_eq!(hash, expected);
}

#[test]
fn webp_without_seek() {
    let bytes = fixture("kWebpLossless");
    let pixels = fixture("kWebpLosslessPixels");
    let (w, h) = fixture_size("kWebpLossless");
    let mut d = WebpDecoder::open(OneByte(&bytes), &opts()).unwrap();
    let mut row = vec![0; w * 3];
    for y in 0..h {
        assert_eq!(d.next_row(&mut row), Ok(true));
        assert_eq!(row, pixels[y * w * 3..(y + 1) * w * 3]);
    }
    for name in ["kWebpLossy", "kWebpAlphaHorizontal"] {
        let bytes = fixture(name);
        assert!(matches!(WebpDecoder::open(OneByte(&bytes), &opts()), Err(Error::Unsupported)));
    }
}

#[test]
fn webp_truncated() {
    for name in ["kWebpLossy", "kWebpLossless", "kWebpAlpha"] {
        let bytes = fixture(name);
        let err = match WebpDecoder::open(SliceStream::new(&bytes[..bytes.len() - 60]), &opts()) {
            Ok(mut d) => {
                let mut row = [0; 64 * 3];
                drain(|r| d.next_row(r), &mut row).unwrap_err()
            }
            Err(e) => e,
        };
        assert_eq!(err, Error::Truncated, "{name}");
    }
    let bytes = fixture("kWebpLossy");
    assert!(matches!(WebpDecoder::open(SliceStream::new(&bytes[..20]), &opts()), Err(Error::Truncated)));
}

// Wide enough that the rows are read back several chunks at a time, in either
// row order.
fn wide_bmp(w: usize, h: usize, top_down: bool) -> Vec<u8> {
    let stride = (w * 3 + 3) & !3;
    let mut b = vec![0; 54 + stride * h];
    b[..2].copy_from_slice(b"BM");
    b[10] = 54;
    b[14] = 40;
    b[18..22].copy_from_slice(&(w as i32).to_le_bytes());
    let sh = if top_down { -(h as i32) } else { h as i32 };
    b[22..26].copy_from_slice(&sh.to_le_bytes());
    b[26] = 1;
    b[28] = 24;
    for y in 0..h {
        let at = 54 + stride * if top_down { y } else { h - 1 - y };
        for x in 0..w {
            b[at + 3 * x] = (x + y) as u8;
            b[at + 3 * x + 1] = (x >> 4) as u8;
            b[at + 3 * x + 2] = (y * 9) as u8;
        }
    }
    b
}

fn wide_bmp_rows(d: &mut BmpDecoder, w: usize, h: usize) {
    let mut row = vec![0; w * 3];
    for y in 0..h {
        assert_eq!(d.next_row(&mut row), Ok(true));
        for x in 0..w {
            assert_eq!(row[3 * x], (y * 9) as u8);
            assert_eq!(row[3 * x + 1], (x >> 4) as u8);
            assert_eq!(row[3 * x + 2], (x + y) as u8);
        }
    }
    assert_eq!(d.next_row(&mut row), Ok(false));
}

#[test]
fn bmp_chunks() {
    for top_down in [false, true] {
        let b = wide_bmp(3001, 13, top_down);
        let mut d = BmpDecoder::open(SliceStream::new(&b), &opts()).unwrap();
        wide_bmp_rows(&mut d, 3001, 13);
    }
}

#[test]
fn bmp_without_seek() {
    let b = wide_bmp(3001, 13, true);
    let mut d = BmpDecoder::open(OneByte(&b), &opts()).unwrap();
    wide_bmp_rows(&mut d, 3001, 13);
    let b = wide_bmp(3001, 13, false);
    assert!(matches!(BmpDecoder::open(OneByte(&b), &opts()), Err(Error::Unsupported)));
}

#[test]
fn bmp_truncated() {
    let bytes = fixture("kBmp24");
    let (w, _) = fixture_size("kBmp24");
    let mut d = BmpDecoder::open(SliceStream::new(&bytes[..bytes.len() - 20]), &opts()).unwrap();
    let mut row = vec![0; w * 3];
    assert_eq!(d.next_row(&mut row), Err(Error::Truncated));
}

#[test]
fn bytes_per_pixel() {
    assert_eq!(PixelFormat::Gray8.bytes_per_pixel(), 1);
    assert_eq!(PixelFormat::Rgb888.bytes_per_pixel(), 3);
    assert_eq!(PixelFormat::Rgb565.bytes_per_pixel(), 2);
    assert_eq!(PixelFormat::Bgr888.bytes_per_pixel(), 3);
}

fn info(width: u16, height: u16, format: PixelFormat) -> ImageInfo {
    ImageInfo { width, height, format }
}

fn target(width: u16, height: u16) -> ResizeOptions {
    ResizeOptions { target_width: width, target_height: height, ..Default::default() }
}

fn resize(src: &[u8], i: ImageInfo, stride: usize, dst: &mut [u8], dst_stride: usize, o: &ResizeOptions) -> Result<()> {
    resize_buffer(src, i, stride, dst, dst_stride, o, Orientation::None)
}

#[test]
fn resize_rgb888_to_bgr888() {
    let src = [10, 20, 30, 40, 50, 60];
    let mut dst = [0; 6];
    let mut o = ResizeOptions { format: Some(PixelFormat::Bgr888), ..target(2, 1) };
    resize(&src, info(2, 1, PixelFormat::Rgb888), 6, &mut dst, 6, &o).unwrap();
    assert_eq!(dst, [30, 20, 10, 60, 50, 40]);
    let mut back = [0; 6];
    o.format = Some(PixelFormat::Rgb888);
    resize(&dst, info(2, 1, PixelFormat::Bgr888), 6, &mut back, 6, &o).unwrap();
    assert_eq!(back, src);
}

fn jpeg_bytes(src: &[u8], w: u16, h: u16, format: PixelFormat, options: &JpegOptions) -> Vec<u8> {
    let mut out = Vec::new();
    let sink = |data: &[u8]| {
        out.extend_from_slice(data);
        Ok(())
    };
    let encoder = JpegEncoder::new(w, h, format, options, sink).unwrap();
    drop(encode_buffer(encoder, src, 0).unwrap());
    out
}

#[test]
fn jpeg_encode_bgr888_matches_rgb888() {
    let (mut rgb, mut bgr) = ([0; 8 * 8 * 3], [0; 8 * 8 * 3]);
    for i in 0..8 * 8 {
        rgb[3 * i] = (i * 3) as u8;
        rgb[3 * i + 1] = (255 - i) as u8;
        rgb[3 * i + 2] = (i | 0x20) as u8;
        bgr[3 * i] = rgb[3 * i + 2];
        bgr[3 * i + 1] = rgb[3 * i + 1];
        bgr[3 * i + 2] = rgb[3 * i];
    }
    let o = JpegOptions { quality: 90, ..Default::default() };
    let a = jpeg_bytes(&rgb, 8, 8, PixelFormat::Rgb888, &o);
    let b = jpeg_bytes(&bgr, 8, 8, PixelFormat::Bgr888, &o);
    assert!(!a.is_empty());
    assert_eq!(a, b);
}

#[test]
fn resize_output_size() {
    let none = Orientation::None;
    let mut o = target(100, 50);
    assert_eq!(output_size(200, 100, &o, none), Ok((100, 50)));
    o = ResizeOptions { fit: Fit::Contain, ..target(100, 100) };
    assert_eq!(output_size(200, 100, &o, none), Ok((100, 50)));
    o = ResizeOptions { fit: Fit::Contain, ..target(100, 25) };
    assert_eq!(output_size(200, 100, &o, none), Ok((50, 25)));
    assert_eq!(output_size(200, 100, &target(0, 50), none), Err(Error::InvalidArgument));
}

#[test]
fn resize_identity_gray() {
    let src = [10, 20, 30, 40];
    let mut dst = [0; 4];
    resize(&src, info(2, 2, PixelFormat::Gray8), 2, &mut dst, 2, &target(2, 2)).unwrap();
    assert_eq!(dst, src);
}

#[test]
fn resize_downscale_gray_2x() {
    let src: [u8; 16] = core::array::from_fn(|i| (i * 10) as u8);
    let mut dst = [0; 4];
    resize(&src, info(4, 4, PixelFormat::Gray8), 4, &mut dst, 2, &target(2, 2)).unwrap();
    assert_eq!(dst, [25, 45, 105, 125]);
}

#[test]
fn resize_upscale_gray_2x() {
    let src = [0, 100, 200, 220];
    let mut dst = [0; 16];
    resize(&src, info(2, 2, PixelFormat::Gray8), 2, &mut dst, 4, &target(4, 4)).unwrap();
    assert_eq!((dst[0], dst[3], dst[12], dst[15]), (0, 100, 200, 220));
    assert!((30..=36).contains(&dst[1]));
    assert!((64..=70).contains(&dst[2]));
}

#[test]
fn resize_rgb888_downscale() {
    let src = [255, 0, 0, 0, 255, 0, 0, 0, 255, 100, 100, 100];
    let mut dst = [0; 3];
    resize(&src, info(2, 2, PixelFormat::Rgb888), 6, &mut dst, 0, &target(1, 1)).unwrap();
    assert!(dst.iter().all(|v| (87..=89).contains(v)));
}

#[test]
fn resize_rgb565_roundtrip() {
    let src = [0xF8, 0xFC, 0xF8];
    let mut mid = [0; 2];
    let mut o = ResizeOptions { format: Some(PixelFormat::Rgb565), ..target(1, 1) };
    resize(&src, info(1, 1, PixelFormat::Rgb888), 3, &mut mid, 0, &o).unwrap();
    assert_eq!(u16::from_ne_bytes(mid), 0xFFFF);
    let mut back = [0; 3];
    o.format = Some(PixelFormat::Rgb888);
    resize(&mid, info(1, 1, PixelFormat::Rgb565), 2, &mut back, 0, &o).unwrap();
    assert_eq!(back, [0xFF; 3]);
}

#[test]
fn resize_gray_to_rgb888() {
    let mut dst = [0; 3];
    let o = ResizeOptions { format: Some(PixelFormat::Rgb888), ..target(1, 1) };
    resize(&[200], info(1, 1, PixelFormat::Gray8), 1, &mut dst, 0, &o).unwrap();
    assert_eq!(dst, [200; 3]);
}

#[test]
fn resize_rgb888_to_gray() {
    let mut dst = [0; 1];
    let o = ResizeOptions { format: Some(PixelFormat::Gray8), ..target(1, 1) };
    resize(&[100, 200, 50], info(1, 1, PixelFormat::Rgb888), 3, &mut dst, 0, &o).unwrap();
    assert!((165..=171).contains(&dst[0]));
}

#[test]
fn resize_keeps_the_format_by_default() {
    let mut dst = [0; 2];
    resize(&[50, 150], info(2, 1, PixelFormat::Gray8), 0, &mut dst, 0, &target(2, 1)).unwrap();
    assert_eq!(dst, [50, 150]);
}

#[test]
fn resize_mixed_axis() {
    let src = [10, 30, 50, 70, 90, 110, 130, 150];
    let mut dst = [0; 8];
    resize(&src, info(4, 2, PixelFormat::Gray8), 4, &mut dst, 2, &target(2, 4)).unwrap();
    assert_eq!(dst[..2], [20, 60]);
    assert_eq!(dst[6..], [100, 140]);
}

#[test]
fn resize_in_place_downscale() {
    let mut buf: [u8; 16] = core::array::from_fn(|i| (i * 10) as u8);
    resize_in_place(&mut buf, info(4, 4, PixelFormat::Gray8), 4, 2, &target(2, 2)).unwrap();
    assert_eq!(buf[..4], [25, 45, 105, 125]);
}

#[test]
fn resize_in_place_rejects_upscale() {
    let mut buf = [0; 16];
    let r = resize_in_place(&mut buf, info(2, 2, PixelFormat::Gray8), 2, 4, &target(4, 4));
    assert_eq!(r, Err(Error::InvalidArgument));
}

#[test]
fn resize_decoder_png() {
    let bytes = fixture("kPngGrad16");
    let mut d = Decoder::Png(png(&bytes).unwrap());
    let out = resize_decoder(&mut d, &target(8, 8), Orientation::None).unwrap();
    assert_eq!((out.info.width, out.info.height), (8, 8));
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0;
            for yy in 0..2 {
                for xx in 0..2 {
                    sum += ((x * 2 + xx) * 16 + (y * 2 + yy) * 4) & 0xFF;
                }
            }
            assert!(near(out.data[(y * 8 + x) as usize], sum / 4, 1));
        }
    }
}

#[test]
fn resize_decoder_jpeg() {
    let bytes = fixture("kJpgGrayBig");
    let mut d = Decoder::Jpeg(jpeg(&bytes).unwrap());
    let out = resize_decoder(&mut d, &target(16, 16), Orientation::None).unwrap();
    assert_eq!((out.info.width, out.info.height), (16, 16));
    assert!(out.data.iter().all(|v| (90..=110).contains(v)));
}

fn dither_opts(algorithm: DitherAlgorithm, levels: u8, output: DitherOutput) -> DitherOptions {
    DitherOptions { algorithm, levels, output, ..Default::default() }
}

#[test]
fn dither_none_index_n16() {
    let mut dst = [0xFF; 1];
    let o = dither_opts(DitherAlgorithm::None, 16, DitherOutput::Index);
    dither_buffer(&[128], 1, 1, 0, &mut dst, 0, &o).unwrap();
    assert_eq!(dst[0], 8);
}

#[test]
fn dither_none_gray_n16() {
    let mut dst = [0xFF; 1];
    let o = dither_opts(DitherAlgorithm::None, 16, DitherOutput::Gray);
    dither_buffer(&[128], 1, 1, 0, &mut dst, 0, &o).unwrap();
    assert_eq!(dst[0], 136);
}

#[test]
fn dither_none_n2_threshold() {
    let mut dst = [0xFF; 2];
    let o = dither_opts(DitherAlgorithm::None, 2, DitherOutput::Index);
    dither_buffer(&[127, 128], 2, 1, 0, &mut dst, 0, &o).unwrap();
    assert_eq!(dst, [0, 1]);
}

#[test]
fn dither_bayer8_2level_mean() {
    let (src, mut dst) = ([128; 64], [0; 64]);
    let o = dither_opts(DitherAlgorithm::Bayer8, 2, DitherOutput::Index);
    dither_buffer(&src, 8, 8, 0, &mut dst, 0, &o).unwrap();
    assert!(dst.iter().all(|&v| v <= 1));
    let ones = dst.iter().filter(|&&v| v != 0).count();
    assert!((24..=40).contains(&ones));
}

#[test]
fn dither_floyd_steinberg_brackets() {
    let (src, mut dst) = ([128; 32 * 32], [0; 32 * 32]);
    let o = DitherOptions { serpentine: true, ..dither_opts(DitherAlgorithm::FloydSteinberg, 16, DitherOutput::Index) };
    dither_buffer(&src, 32, 32, 0, &mut dst, 0, &o).unwrap();
    assert!(dst.iter().all(|&v| v == 7 || v == 8));
    let sum: u32 = dst.iter().map(|&v| u32::from(v)).sum();
    assert!((7400..=8000).contains(&sum));
}

#[test]
fn dither_floyd_steinberg_gray_brackets() {
    let (src, mut dst) = ([128; 16 * 16], [0; 16 * 16]);
    let o = DitherOptions { serpentine: true, ..dither_opts(DitherAlgorithm::FloydSteinberg, 16, DitherOutput::Gray) };
    dither_buffer(&src, 16, 16, 0, &mut dst, 0, &o).unwrap();
    assert!(dst.iter().all(|&v| v == 119 || v == 136));
}

#[test]
fn dither_in_place_matches_buffer() {
    let mut buf: [u8; 16] = core::array::from_fn(|i| (i * 16) as u8);
    let o = dither_opts(DitherAlgorithm::None, 16, DitherOutput::Index);
    dither_in_place(&mut buf, 16, 1, 0, &o).unwrap();
    for (i, &v) in buf.iter().enumerate() {
        assert_eq!(usize::from(v), (((i * 16) * 15 * 2 + 255) / 510).min(15));
    }

    let src: [u8; 64] = core::array::from_fn(|i| (i * 4) as u8);
    let o = DitherOptions { serpentine: true, ..dither_opts(DitherAlgorithm::Stucki, 4, DitherOutput::Gray) };
    let mut expected = [0; 64];
    dither_buffer(&src, 8, 8, 0, &mut expected, 0, &o).unwrap();
    let mut buf = src;
    dither_in_place(&mut buf, 8, 8, 0, &o).unwrap();
    assert_eq!(buf, expected);
}

#[test]
fn dither_row_by_row() {
    let src = [0, 50, 100, 200, 20, 70, 120, 220];
    let mut got = [0; 8];
    let o = dither_opts(DitherAlgorithm::None, 4, DitherOutput::Index);
    let mut d = Ditherer::new(4, &o).unwrap();
    assert_eq!(d.levels(), 4);
    assert_eq!(d.output(), DitherOutput::Index);
    for y in 0..2 {
        d.process_row(&src[y * 4..], &mut got[y * 4..]).unwrap();
    }
    assert_eq!(got, [0, 1, 1, 2, 0, 1, 1, 3]);
}

#[test]
fn dither_rejects_zero_width() {
    assert!(matches!(Ditherer::new(0, &DitherOptions::default()), Err(Error::InvalidArgument)));
}

fn raw_encode(format: RawFormat, src: &[u8], w: u16, h: u16, dst: &mut [u8]) -> Result<usize> {
    encode_buffer(RawEncoder::new(format, w, h, dst)?, src, 0)
}

#[test]
fn raw_stride() {
    assert_eq!(RawFormat::L8.stride(10), 10);
    assert_eq!(RawFormat::I4.stride(10), 5);
    assert_eq!(RawFormat::I4.stride(11), 6);
    assert_eq!(RawFormat::I1.stride(10), 2);
    assert_eq!(RawFormat::I1.stride(7), 1);
    assert_eq!(RawFormat::I1.stride(8), 1);
    assert_eq!(RawFormat::I1.stride(9), 2);
    assert_eq!(RawFormat::Rgb888.stride(4), 12);
    assert_eq!(RawFormat::Rgb565.stride(4), 8);
}

#[test]
fn raw_input_format() {
    assert_eq!(RawFormat::L8.input_format(), PixelFormat::Gray8);
    assert_eq!(RawFormat::I4.input_format(), PixelFormat::Gray8);
    assert_eq!(RawFormat::I1.input_format(), PixelFormat::Gray8);
    assert_eq!(RawFormat::Rgb888.input_format(), PixelFormat::Rgb888);
    assert_eq!(RawFormat::Rgb565.input_format(), PixelFormat::Rgb565);
}

#[test]
fn raw_l8_passthrough() {
    let src = [10, 20, 30, 40, 50, 60];
    let mut dst = [0; 6];
    assert_eq!(raw_encode(RawFormat::L8, &src, 3, 2, &mut dst), Ok(6));
    assert_eq!(dst, src);
}

#[test]
fn raw_i4_pack() {
    let src = [0xA, 0xB, 0xC, 0xD, 0xE, 0x1, 0x2, 0x3, 0x4, 0x5];
    let mut dst = [0; 6];
    assert_eq!(raw_encode(RawFormat::I4, &src, 5, 2, &mut dst), Ok(6));
    assert_eq!(dst, [0xAB, 0xCD, 0xE0, 0x12, 0x34, 0x50]);
}

#[test]
fn raw_i1_pack() {
    let src = [1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 1, 0, 1, 0, 1, 0, 0, 1];
    let mut dst = [0; 4];
    assert_eq!(raw_encode(RawFormat::I1, &src, 11, 2, &mut dst), Ok(4));
    assert_eq!(dst, [0xAA, 0xC0, 0x55, 0x20]);
}

#[test]
fn raw_rgb_passthrough() {
    let src = [1, 2, 3, 4, 5, 6];
    let mut dst = [0; 6];
    assert_eq!(raw_encode(RawFormat::Rgb888, &src, 2, 1, &mut dst), Ok(6));
    assert_eq!(dst, src);
    let src: Vec<u8> = [0xF800u16, 0x07E0].iter().flat_map(|v| v.to_ne_bytes()).collect();
    let mut dst = [0; 4];
    assert_eq!(raw_encode(RawFormat::Rgb565, &src, 2, 1, &mut dst), Ok(4));
    assert_eq!(dst[..], src);
}

#[test]
fn raw_buffer_too_small() {
    let mut dst = [0; 2];
    assert_eq!(raw_encode(RawFormat::L8, &[1, 2, 3, 4], 4, 1, &mut dst), Err(Error::InvalidArgument));
}

#[test]
fn raw_row_by_row() {
    let mut dst = [0; 4];
    let mut e = RawEncoder::new(RawFormat::I4, 4, 2, &mut dst).unwrap();
    assert_eq!(e.stride(), 2);
    assert_eq!(e.buffer_size(), 4);
    assert_eq!(e.info().format, PixelFormat::Gray8);
    e.push_row(&[0x1, 0x2, 0x3, 0x4]).unwrap();
    e.push_row(&[0xF, 0xE, 0xD, 0xC]).unwrap();
    assert_eq!(e.finish(), Ok(4));
    assert_eq!(dst, [0x12, 0x34, 0xFE, 0xDC]);
}

#[test]
fn raw_finish_before_full_rejected() {
    let mut dst = [0; 8];
    let mut e = RawEncoder::new(RawFormat::L8, 4, 2, &mut dst).unwrap();
    e.push_row(&[1, 2, 3, 4]).unwrap();
    assert_eq!(e.finish(), Err(Error::InvalidState));
}

#[test]
fn dither_chains_into_i4() {
    let src: [u8; 16] = core::array::from_fn(|i| (i * 17) as u8);
    let mut indices = [0; 16];
    let o = dither_opts(DitherAlgorithm::FloydSteinberg, 16, DitherOutput::Index);
    dither_buffer(&src, 16, 1, 0, &mut indices, 0, &o).unwrap();
    assert!(indices.iter().enumerate().all(|(i, &v)| usize::from(v) == i));
    let mut i4 = [0; 8];
    assert_eq!(raw_encode(RawFormat::I4, &indices, 16, 1, &mut i4), Ok(8));
    assert!(i4.iter().enumerate().all(|(i, &v)| usize::from(v) == ((i * 2) << 4 | (i * 2 + 1))));
}

// Encodes, decodes it back with the JPEG decoder, and returns the decoded rows
// with the encoded size.
fn roundtrip(src: &[u8], w: u16, h: u16, format: PixelFormat, quality: u8, subsampling: Subsampling) -> (Vec<u8>, usize) {
    let options = JpegOptions { quality, subsampling, ..Default::default() };
    let bytes = jpeg_bytes(src, w, h, format, &options);
    assert_eq!(bytes[..2], [0xFF, 0xD8]);
    assert_eq!(bytes[bytes.len() - 2..], [0xFF, 0xD9]);
    let mut d = jpeg(&bytes).unwrap();
    assert_eq!((d.info().width, d.info().height), (w, h));
    let stride = d.info().row_bytes();
    let mut out = vec![0; stride * usize::from(h)];
    for row in out.chunks_exact_mut(stride) {
        assert_eq!(d.next_row(row), Ok(true));
    }
    (out, bytes.len())
}

#[test]
fn jpeg_encode_gray_flat() {
    let (out, _) = roundtrip(&[128; 16 * 16], 16, 16, PixelFormat::Gray8, 90, Subsampling::Default);
    assert!(out.iter().all(|&v| near(v, 128, 5)));
}

#[test]
fn jpeg_encode_gray_split() {
    let src: Vec<u8> = (0..16 * 16).map(|i| if i % 16 < 8 { 40 } else { 220 }).collect();
    let (out, _) = roundtrip(&src, 16, 16, PixelFormat::Gray8, 90, Subsampling::Default);
    assert!(near(out[8 * 16 + 2], 40, 25));
    assert!(near(out[8 * 16 + 13], 220, 25));
}

#[test]
fn jpeg_encode_rgb_red() {
    let src: Vec<u8> = [220, 30, 30].repeat(16 * 16);
    let (out, _) = roundtrip(&src, 16, 16, PixelFormat::Rgb888, 90, Subsampling::Default);
    let c = (8 * 16 + 8) * 3;
    assert!(near(out[c], 220, 25));
    assert!(near(out[c + 1], 30, 30));
    assert!(near(out[c + 2], 30, 30));
}

#[test]
fn jpeg_encode_pads_to_whole_blocks() {
    let mut src = vec![0u8; 17 * 9];
    for y in 0..9 {
        for x in 0..17 {
            src[y * 17 + x] = ((x + y) * 13) as u8;
        }
    }
    let (out, _) = roundtrip(&src, 17, 9, PixelFormat::Gray8, 95, Subsampling::Default);
    let err: i32 = out.iter().zip(&src).map(|(&a, &b)| (i32::from(a) - i32::from(b)).abs()).sum();
    assert!(err / (17 * 9) <= 12);
}

#[test]
fn jpeg_encode_low_quality_is_smaller() {
    let mut src = vec![0u8; 32 * 32 * 3];
    for y in 0..32 {
        for x in 0..32 {
            src[(y * 32 + x) * 3] = (x * 8) as u8;
            src[(y * 32 + x) * 3 + 1] = (y * 8) as u8;
            src[(y * 32 + x) * 3 + 2] = ((x + y) * 4) as u8;
        }
    }
    let size = |quality| jpeg_bytes(&src, 32, 32, PixelFormat::Rgb888, &JpegOptions { quality, ..Default::default() }).len();
    let (hi, lo) = (size(95), size(20));
    assert!(lo < hi && lo > 0);
}

#[test]
fn jpeg_encode_subsampling() {
    let mut src = vec![0u8; 16 * 16 * 3];
    for y in 0..16 {
        for x in 0..16 {
            src[(y * 16 + x) * 3] = (180 + x) as u8;
            src[(y * 16 + x) * 3 + 1] = (60 + y) as u8;
            src[(y * 16 + x) * 3 + 2] = (40 + ((x + y) >> 1)) as u8;
        }
    }
    let center = (8 * 16 + 8) * 3;
    let (out, s444) = roundtrip(&src, 16, 16, PixelFormat::Rgb888, 90, Subsampling::S444);
    assert!(near(out[center], 188, 30) && near(out[center + 1], 68, 30));
    let (out, s422) = roundtrip(&src, 16, 16, PixelFormat::Rgb888, 90, Subsampling::S422);
    assert!(near(out[center], 188, 30));
    let (out, s420) = roundtrip(&src, 16, 16, PixelFormat::Rgb888, 90, Subsampling::S420);
    assert!(near(out[center], 188, 30));
    assert!(s420 <= s422 && s422 <= s444);
}

#[test]
fn jpeg_encode_subsampling_padding() {
    let src: Vec<u8> = (0..17 * 17 * 3).map(|i| i as u8).collect();
    for s in [Subsampling::S444, Subsampling::S422, Subsampling::S420] {
        roundtrip(&src, 17, 17, PixelFormat::Rgb888, 80, s);
    }
}

#[test]
fn jpeg_encode_gray_ignores_subsampling() {
    let src: Vec<u8> = (0..16 * 16).map(|i| ((i * 13) & 0xFF) as u8).collect();
    let a = roundtrip(&src, 16, 16, PixelFormat::Gray8, 80, Subsampling::S444);
    let b = roundtrip(&src, 16, 16, PixelFormat::Gray8, 80, Subsampling::S420);
    assert_eq!(a, b);
}

#[test]
fn jpeg_encode_info() {
    let mut sink = |_: &[u8]| Ok(());
    for format in [PixelFormat::Rgb888, PixelFormat::Gray8, PixelFormat::Rgb565] {
        let e = JpegEncoder::new(8, 8, format, &JpegOptions::default(), &mut sink).unwrap();
        assert_eq!(e.info().format, format);
    }
    assert!(jpeg_buffer_upper_bound(8, 8, PixelFormat::Rgb888) > 0);
}

#[test]
fn jpeg_encode_rgb565_red() {
    let red = (27u16 << 11) | (7 << 5) | 3;
    let src: Vec<u8> = red.to_ne_bytes().repeat(16 * 16);
    let (out, _) = roundtrip(&src, 16, 16, PixelFormat::Rgb565, 90, Subsampling::Default);
    let c = (8 * 16 + 8) * 3;
    assert!(near(out[c], 220, 25));
    assert!(near(out[c + 1], 30, 30));
    assert!(near(out[c + 2], 30, 30));
}

fn pattern(w: usize, h: usize) -> Vec<u8> {
    let mut src = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let p = &mut src[(y * w + x) * 3..];
            p[0] = (x * 5) as u8;
            p[1] = (y * 6) as u8;
            p[2] = ((x ^ y) * 3) as u8;
        }
    }
    src
}

#[test]
fn jpeg_slice_sink_matches_a_closure_in_staged_chunks() {
    let src = pattern(48, 40);
    let options = JpegOptions::default();
    let mut buf = vec![0; jpeg_buffer_upper_bound(48, 40, PixelFormat::Rgb888)];
    let e = JpegEncoder::new(48, 40, PixelFormat::Rgb888, &options, SliceSink::new(&mut buf)).unwrap();
    let (written, _) = encode_buffer(e, &src, 0).unwrap();

    let mut out = Vec::new();
    let mut calls = 0;
    let mut max_chunk = 0;
    let sink = |data: &[u8]| {
        out.extend_from_slice(data);
        calls += 1;
        max_chunk = max_chunk.max(data.len());
        Ok(())
    };
    let e = JpegEncoder::new(48, 40, PixelFormat::Rgb888, &options, sink).unwrap();
    let (total, _) = encode_buffer(e, &src, 0).unwrap();
    assert_eq!(total, written);
    assert_eq!(out, buf[..written]);
    assert!(calls > 1 && max_chunk < written);
}

#[test]
fn jpeg_sink_error_is_sticky() {
    let mut room = 64;
    let sink = |data: &[u8]| {
        if data.len() > room {
            return Err(Error::Io);
        }
        room -= data.len();
        Ok(())
    };
    let mut e = JpegEncoder::new(64, 64, PixelFormat::Gray8, &JpegOptions::default(), sink);
    let mut failed = false;
    if let Ok(e) = &mut e {
        for y in 0..64u8 {
            let row: [u8; 64] = core::array::from_fn(|x| (x as u8 * 3) ^ (y * 5));
            if e.push_row(&row).is_err() {
                failed = true;
                break;
            }
        }
    }
    let finished = e.and_then(|e| e.finish().map(|_| ()));
    assert!(failed || finished == Err(Error::Io));
}

#[test]
fn jpeg_slice_sink_overflow_is_out_of_memory() {
    let mut buf = [0; 16];
    let e = JpegEncoder::new(8, 8, PixelFormat::Gray8, &JpegOptions::default(), SliceSink::new(&mut buf)).unwrap();
    let r = encode_buffer(e, &[128; 64], 0);
    assert!(matches!(r, Err(Error::OutOfMemory)));
}

#[test]
fn jpeg_encode_stream_matches_buffer() {
    const W: usize = 24;
    const H: usize = 20;
    let src: Vec<u8> = (0..W * H).map(|i| (i * 7) as u8).collect();
    let o = JpegOptions::default();
    let reference = jpeg_bytes(&src, W as u16, H as u16, PixelFormat::Gray8, &o);

    let mut out = Vec::new();
    let sink = |data: &[u8]| {
        out.extend_from_slice(data);
        Ok(())
    };
    let e = JpegEncoder::new(W as u16, H as u16, PixelFormat::Gray8, &o, sink).unwrap();
    let mut row = [0; W];
    let (n, _) = encode_stream(e, &mut SliceStream::new(&src), &mut row).unwrap();
    assert_eq!(n, reference.len());
    assert_eq!(out, reference);

    let sink = |_: &[u8]| Ok(());
    let e = JpegEncoder::new(W as u16, H as u16, PixelFormat::Gray8, &o, sink).unwrap();
    let r = encode_stream(e, &mut SliceStream::new(&src[..src.len() - 1]), &mut row);
    assert!(matches!(r, Err(Error::Truncated)));
}

#[test]
fn resizer_row_by_row() {
    let src: [u8; 16] = core::array::from_fn(|i| (i * 10) as u8);
    let mut r = Resizer::new(info(4, 4, PixelFormat::Gray8), &target(2, 2)).unwrap();
    assert_eq!((r.info().width, r.info().height), (2, 2));
    let mut got = [0; 4];
    let mut at = 0;
    for sy in 0..4 {
        let mut ready = r.push_row(&src[sy * 4..]).unwrap();
        while ready.pop(&mut got[at * 2..]).unwrap() {
            at += 1;
        }
    }
    let mut finished = r.finish();
    while at < 2 && finished.pop(&mut got[at * 2..]).unwrap() {
        at += 1;
    }
    assert_eq!(at, 2);
    assert_eq!(got, [25, 45, 105, 125]);
}

#[test]
fn resizer_rejects_a_row_past_the_last() {
    let mut r = Resizer::new(info(1, 1, PixelFormat::Gray8), &target(1, 1)).unwrap();
    let mut ready = r.push_row(&[7]).unwrap();
    let mut out = [0];
    assert_eq!(ready.pop(&mut out), Ok(true));
    assert!(matches!(r.push_row(&[7]), Err(Error::InvalidState)));
}

const ORIENTATIONS: [Orientation; 8] = [
    Orientation::None,
    Orientation::MirrorX,
    Orientation::Rot180,
    Orientation::MirrorY,
    Orientation::Transpose,
    Orientation::Rot90,
    Orientation::Transverse,
    Orientation::Rot270,
];

fn oriented_xy(o: Orientation, w: usize, h: usize, x: usize, y: usize) -> (usize, usize) {
    match o {
        Orientation::None => (x, y),
        Orientation::MirrorX => (w - 1 - x, y),
        Orientation::Rot180 => (w - 1 - x, h - 1 - y),
        Orientation::MirrorY => (x, h - 1 - y),
        Orientation::Transpose => (y, x),
        Orientation::Rot90 => (h - 1 - y, x),
        Orientation::Transverse => (h - 1 - y, w - 1 - x),
        Orientation::Rot270 => (y, w - 1 - x),
    }
}

#[test]
fn resize_orientation() {
    const W: usize = 3;
    const H: usize = 2;
    let src: [u8; W * H * 3] = core::array::from_fn(|i| (i * 7 + 1) as u8);
    for o in ORIENTATIONS {
        let (ow, oh) = if o.swaps_axes() { (H, W) } else { (W, H) };
        let opts = target(ow as u16, oh as u16);
        let gray: [u8; W * H] = core::array::from_fn(|i| src[i]);
        let mut got = [0; W * H];
        resize_buffer(&gray, info(3, 2, PixelFormat::Gray8), W, &mut got, 0, &opts, o).unwrap();
        let mut rgb = [0; W * H * 3];
        resize_buffer(&src, info(3, 2, PixelFormat::Rgb888), W * 3, &mut rgb, 0, &opts, o).unwrap();
        for y in 0..H {
            for x in 0..W {
                let (ox, oy) = oriented_xy(o, W, H, x, y);
                assert_eq!(got[oy * ow + ox], gray[y * W + x]);
                assert_eq!(rgb[(oy * ow + ox) * 3..][..3], src[(y * W + x) * 3..][..3]);
            }
        }
    }

    let opts = ResizeOptions { fit: Fit::Contain, ..target(100, 100) };
    assert_eq!(output_size(200, 100, &opts, Orientation::Rot90), Ok((50, 100)));

    let opts = ResizeOptions { fit: Fit::Contain, ..target(3, 3) };
    let r = OrientedResizer::new(info(6, 3, PixelFormat::Rgb565), &opts, Orientation::Rot90).unwrap();
    assert_eq!((r.info().width, r.info().height), (2, 3));
}

#[test]
fn pipeline_backpressure() {
    let options = PipelineOptions { row_bytes: 16, slots: 3, ..Default::default() };
    let (mut sent, mut got) = (0, 0);
    pipeline::run(
        &options,
        |ring: &Ring| {
            for i in 0..200 {
                let row: [u8; 16] = core::array::from_fn(|k| (i * 7 + k) as u8);
                ring.put(&row)?;
                sent += 1;
            }
            Ok(())
        },
        |ring: &Ring| {
            let mut row = [0; 16];
            while ring.get(&mut row)? {
                assert!(row.iter().enumerate().all(|(k, &v)| v == (got * 7 + k) as u8));
                got += 1;
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!((sent, got), (200, 200));
}

#[test]
fn pipeline_start_and_poll() {
    let options = PipelineOptions { row_bytes: 16, slots: 3, ..Default::default() };
    let got = Arc::new(AtomicUsize::new(0));
    let counter = got.clone();
    let job = pipeline::start(
        &options,
        |ring: &Ring| {
            for i in 0..200 {
                ring.put(&[i as u8; 16])?;
            }
            Ok(())
        },
        move |ring: &Ring| {
            let mut row = [0; 16];
            while ring.get(&mut row)? {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        },
    )
    .unwrap();
    while !job.is_finished() {
        idf::thread::yield_now();
    }
    assert_eq!(job.join(), Ok(()));
    assert_eq!(got.load(Ordering::Relaxed), 200);
}

#[test]
fn pipeline_producer_error_wins() {
    let options = PipelineOptions { row_bytes: 16, ..Default::default() };
    let r = pipeline::run(
        &options,
        |_: &Ring| Err(Error::Truncated),
        |ring: &Ring| {
            let mut row = [0; 16];
            while ring.get(&mut row)? {}
            Ok(())
        },
    );
    assert_eq!(r, Err(Error::Truncated));
}

#[test]
fn pipeline_decoder_resize_chain() {
    let bytes = fixture("kPngGrad16");
    let opts_r = target(8, 8);
    let mut out = [0; 8 * 8];
    let options = PipelineOptions { row_bytes: 8, slots: 4, ..Default::default() };
    let bytes_ref = &bytes;
    pipeline::run(
        &options,
        |ring: &Ring| {
            let mut d = png(bytes_ref)?;
            let mut r = Resizer::new(d.info(), &opts_r)?;
            let (mut src, mut dst) = ([0; 16], [0; 8]);
            for _ in 0..16 {
                assert!(d.next_row(&mut src)?);
                let mut ready = r.push_row(&src)?;
                while ready.pop(&mut dst)? {
                    ring.put(&dst)?;
                }
            }
            let mut last = r.finish();
            while last.pop(&mut dst)? {
                ring.put(&dst)?;
            }
            Ok(())
        },
        |ring: &Ring| {
            let mut y = 0;
            let mut row = [0; 8];
            while ring.get(&mut row)? {
                out[y * 8..(y + 1) * 8].copy_from_slice(&row);
                y += 1;
            }
            assert_eq!(y, 8);
            Ok(())
        },
    )
    .unwrap();

    let mut full = [0; 16 * 16];
    let mut d = png(&bytes).unwrap();
    for row in full.chunks_exact_mut(16) {
        assert_eq!(d.next_row(row), Ok(true));
    }
    let mut expected = [0; 8 * 8];
    resize(&full, info(16, 16, PixelFormat::Gray8), 16, &mut expected, 8, &opts_r).unwrap();
    assert_eq!(out, expected);
}

fn recolored(r: &Recolor, src: &[u8], format: PixelFormat) -> Vec<u8> {
    let n = src.len() / format.bytes_per_pixel();
    let (mut intensity, mut out) = (vec![0; n], vec![0; n]);
    r.to_intensity(src, format, &mut intensity).unwrap();
    r.finalize(&intensity, &mut out);
    out
}

#[test]
fn recolor_gray_passes_through_srgb() {
    let r = Recolor::new(&RecolorOptions::default());
    let out = recolored(&r, &[0, 128, 255], PixelFormat::Gray8);
    assert_eq!(out[0], 0);
    assert!(near(out[1], 128, 2));
    assert_eq!(out[2], 255);
}

#[test]
fn recolor_luma_ordering() {
    let r = Recolor::new(&RecolorOptions::default());
    let f = |p: [u8; 3]| recolored(&r, &p, PixelFormat::Rgb888)[0];
    let (red, green, blue) = (f([200, 0, 0]), f([0, 200, 0]), f([0, 0, 200]));
    assert!(green > red && red > blue);
}

#[test]
fn recolor_linear_vs_perceptual() {
    let linear = Recolor::new(&RecolorOptions::default());
    let perceptual = Recolor::new(&RecolorOptions { linear_downsample: false, ..Default::default() });
    let averaged = |r: &Recolor| {
        let mut i = [0; 2];
        r.to_intensity(&[0, 255], PixelFormat::Gray8, &mut i).unwrap();
        let mut avg = [((u16::from(i[0]) + u16::from(i[1])) / 2) as u8];
        r.finalize_in_place(&mut avg);
        avg[0]
    };
    let (gl, gp) = (averaged(&linear), averaged(&perceptual));
    assert!(gl > gp);
    assert!((110..=145).contains(&gp));
    assert!(gl >= 170);
}

#[test]
fn recolor_invert() {
    let r = Recolor::new(&RecolorOptions { invert: true, ..Default::default() });
    assert_eq!(recolored(&r, &[0, 255], PixelFormat::Gray8), [255, 0]);
}

#[test]
fn recolor_epd_lut() {
    let lut: [u8; 16] = core::array::from_fn(|i| (i * 255 / 15) as u8);
    let r = Recolor::new(&RecolorOptions { out_gamma: GammaOut::EpdLut(lut), in_gamma: GammaIn::Linear, ..Default::default() });
    let out = recolored(&r, &[0, 128, 255], PixelFormat::Gray8);
    assert_eq!(out[0], 0);
    assert!(near(out[1], 128, 4));
    assert_eq!(out[2], 255);
}
