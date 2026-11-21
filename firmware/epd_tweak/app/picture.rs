/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Whole-panel L8 pictures: the test pattern and an uploaded image.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;

use devkit_gfx::{ClipStore, Color, DisplayList, Font, FontChain, GlyphCache, Prepared, Rect, Rgb565, Size, Surface, TextOptions, TextRun};
use devkit_image::{DecodeOptions, DitherAlgorithm, DitherOptions, DitherOutput, Fit, Orientation, ResizeOptions, SliceStream};
use idf::heap::{self, Caps};

use crate::resources;

const LEVELS: usize = 16;
const TICK_W: usize = 2;
const TICK_H: usize = 10;
const GLYPH_CACHE: usize = 16 * 1024;

static LABEL_FONTS: [&Font; 1] = [&resources::LABEL_32];

pub(crate) struct Picture {
    pub size: Size,
    pub pixels: Box<[u8], Caps>,
}

fn blank(size: Size, gray: u8) -> Result<Picture, String> {
    let len = usize::from(size.width) * usize::from(size.height);
    let mut pixels = heap::try_vec(len, Caps::SPIRAM).map_err(|err| format!("no mem: {err}"))?;
    pixels.resize(len, gray);
    Ok(Picture { size, pixels: pixels.into_boxed_slice() })
}

fn dither(options: DitherOptions) -> DitherOptions {
    DitherOptions { algorithm: DitherAlgorithm::FloydSteinberg, levels: LEVELS as u8, serpentine: true, output: DitherOutput::Gray, ..options }
}

/// 16 strips of the levels over a dithered gradient, with marks where the
/// gradient passes each level.
pub(crate) fn pattern(size: Size) -> Result<Picture, String> {
    let mut picture = blank(size, 0xFF)?;
    let (w, h) = (usize::from(size.width), usize::from(size.height));
    let top = h / 2;
    let strip = w / LEVELS;
    let px = &mut picture.pixels;
    for i in 0..LEVELS {
        let level = (i * 0x11) as u8;
        let x1 = if i == LEVELS - 1 { w } else { (i + 1) * strip };
        for y in 0..top {
            px[y * w + i * strip..y * w + x1].fill(level);
        }
    }

    let bottom = &mut px[top * w..];
    for x in 0..w {
        bottom[x] = (x * 255 / (w - 1)) as u8;
    }
    for y in 1..h - top {
        bottom.copy_within(0..w, y * w);
    }
    devkit_image::dither_in_place(bottom, w as u16, (h - top) as u16, w, &dither(DitherOptions::default())).map_err(|err| format!("dither: {err:?}"))?;
    let rows = h - top;
    for i in 0..LEVELS {
        let cx = i * (w - 1) / (LEVELS - 1);
        for dy in 0..TICK_H {
            let gray = if dy < TICK_H / 2 { 0x00 } else { 0xFF };
            for x in (cx.saturating_sub(TICK_W / 2)..cx + TICK_W - TICK_W / 2).filter(|&x| x < w) {
                bottom[dy * w + x] = gray;
                bottom[(rows - TICK_H + dy) * w + x] = gray;
            }
        }
    }

    let chain = FontChain::new(&LABEL_FONTS);
    let mut cache = GlyphCache::new(GLYPH_CACHE);
    for i in 0..LEVELS {
        let level = (i * 0x11) as u8;
        let ink = if level < 0x80 { 0xFF } else { 0x00 };
        let mut run = TextRun::new();
        run.layout(&chain, &format!("{i}"), &TextOptions::default());
        let x = i * strip + (strip - run.width() as usize) / 2;
        let y = (top - run.height() as usize) / 2;
        label(&mut picture.pixels, w, (x, y), &run, &mut cache, level, ink);
    }
    Ok(picture)
}

/// Draws `run` white on black at 16 bits, and takes that as its coverage.
fn label(px: &mut [u8], stride: usize, (x, y): (usize, usize), run: &TextRun, cache: &mut GlyphCache, paper: u8, ink: u8) {
    let (rw, rh) = (run.width() as usize, run.height() as usize);
    let area = Rect::new(0, 0, rw as i16, rh as i16);
    let mut list = DisplayList::new();
    list.text(0, 0, run, Color::rgb(0xFF, 0xFF, 0xFF));
    let mut prepared = Prepared::new();
    prepared.prepare(&list, &[area], cache);
    let mut scratch = vec![0u16; rw * rh];
    let surface = unsafe { Surface::new(scratch.as_mut_ptr().cast(), rw * 2, area) };
    devkit_gfx::draw::<Rgb565, _, _, _>(&list, &prepared, &mut ClipStore::new(), &surface);
    for (row, src) in scratch.chunks_exact(rw).enumerate() {
        let dst = &mut px[(y + row) * stride + x..][..rw];
        for (d, &s) in dst.iter_mut().zip(src) {
            let cover = i32::from((s >> 5) & 0x3F) * 255 / 63;
            *d = (i32::from(paper) + (i32::from(ink) - i32::from(paper)) * cover / 255) as u8;
        }
    }
}

/// `data` fitted inside the panel, dithered to 16 levels, centered on white.
/// Returns the picture and the size the image took.
pub(crate) fn image(size: Size, data: &[u8]) -> Result<(Picture, (u16, u16)), String> {
    let (pw, ph) = (size.width, size.height);
    let decode = DecodeOptions { target_width: pw, target_height: ph, caps: Some(Caps::SPIRAM), ..Default::default() };
    let mut decoder = devkit_image::open(SliceStream::new(data), &decode).map_err(|err| format!("open: {err:?}"))?;
    let resize = ResizeOptions {
        target_width: pw,
        target_height: ph,
        fit: Fit::Contain,
        format: Some(devkit_gfx::ImageFormat::L8),
        caps: Some(Caps::SPIRAM),
    };
    let info = decoder.info();
    let (w, h) = devkit_image::output_size(info.width, info.height, &resize, Orientation::None).map_err(|err| format!("size: {err:?}"))?;
    let mut picture = blank(size, 0xFF)?;
    let stride = usize::from(pw);
    let offset = usize::from((ph - h) / 2) * stride + usize::from((pw - w) / 2);
    let region = &mut picture.pixels[offset..];
    devkit_image::resize_decoder_into(&mut decoder, region, stride, &resize, Orientation::None).map_err(|err| format!("resize: {err:?}"))?;
    devkit_image::dither_in_place(region, w, h, stride, &dither(DitherOptions::default())).map_err(|err| format!("dither: {err:?}"))?;
    Ok((picture, (w, h)))
}
