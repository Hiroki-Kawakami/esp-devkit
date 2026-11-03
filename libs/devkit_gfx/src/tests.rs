/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::alloc::Global;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::format::{self, FontHeader, GlyphRecord};
use crate::*;

const SOLID: char = 'A';
const HOLLOW: char = 'B';
const WIDE: char = '漢';

fn build_font(glyphs: &[(char, u8, u8, u16)], compress: bool) -> &'static Font {
    let mut sorted = glyphs.to_vec();
    sorted.sort_by_key(|g| g.0);
    let mut records = Vec::new();
    let mut data = Vec::new();
    for &(c, w, h, advance) in &sorted {
        let (w8, h8) = (usize::from(w), usize::from(h));
        let levels: Vec<u8> = (0..w8 * h8)
            .map(|i| if c == HOLLOW && i % w8 != 0 && i >= w8 { 0 } else { 15 - (i % 3) as u8 })
            .collect();
        let bitmap = data.len() as u32;
        let bitmap = if compress {
            data.extend(format::rle_encode(&format::prefilter_rows(&levels, w8), 4));
            bitmap | format::GLYPH_COMPRESSED
        } else {
            data.extend(format::pack_levels(&levels, 4));
            bitmap
        };
        GlyphRecord { bitmap, advance, width: w, height: h, ofs_x: 0, ofs_y: -2 }.write(&mut records);
    }
    data.extend_from_slice(&[0; format::FONT_DATA_SLACK]);
    let codepoints = format::FONT_HEADER_SIZE as u32;
    let table = codepoints + sorted.len() as u32 * 4;
    let mut out = Vec::new();
    FontHeader {
        bpp: 4,
        flags: if compress { format::FONT_PREFILTER } else { 0 },
        px: 12,
        line_height: 14,
        base_line: 3,
        max_ascent: 10,
        max_descent: 2,
        underline_position: -1,
        underline_thickness: 1,
        glyph_count: sorted.len() as u32,
        codepoints,
        glyphs: table,
        data: table + records.len() as u32,
        data_len: data.len() as u32,
    }
    .write(&mut out);
    for g in &sorted {
        out.extend_from_slice(&(g.0 as u32).to_le_bytes());
    }
    out.extend_from_slice(&records);
    out.extend_from_slice(&data);
    Box::leak(Box::new(Font::parse(Vec::leak(out))))
}

fn test_chain() -> FontChain {
    let font = build_font(&[(SOLID, 8, 10, 160), (HOLLOW, 8, 10, 160), (' ', 0, 0, 80), ('.', 2, 2, 48), (WIDE, 12, 12, 256)], true);
    FontChain::new(Vec::leak(vec![font]))
}

fn layout(chain: &FontChain, text: &str, options: TextOptions) -> TextRun {
    let mut run = TextRun::new();
    run.layout(chain, text, &options);
    run
}

fn render<P: Pixel>(list: &DisplayList<'_>, cache: &mut GlyphCache, width: i16, height: i16, parts: &[Rect]) -> Vec<u8> {
    let stride = width as usize * P::BYTES;
    let mut out = vec![0u8; stride * height as usize];
    let mut prepared = Prepared::new();
    prepared.prepare(list, parts, cache);
    for part in parts {
        let part_stride = part.w as usize * P::BYTES;
        let mut pixels = vec![0u16; (part_stride * part.h as usize).div_ceil(2)];
        let surface = unsafe { Surface::new(pixels.as_mut_ptr().cast(), part_stride, *part) };
        draw::<P, _, _, _>(list, &prepared, &mut ClipStore::new(), &surface);
        let bytes = unsafe { core::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), part_stride * part.h as usize) };
        for row in 0..part.h as usize {
            let dst = (part.y as usize + row) * stride + part.x as usize * P::BYTES;
            out[dst..dst + part_stride].copy_from_slice(&bytes[row * part_stride..(row + 1) * part_stride]);
        }
    }
    out
}

fn px565(buffer: &[u8], width: usize, x: usize, y: usize) -> u16 {
    let at = (y * width + x) * 2;
    u16::from_le_bytes([buffer[at], buffer[at + 1]])
}

struct Scene {
    list_runs: (TextRun, TextRun),
    thumb: Vec<u8>,
    corner: CornerMask,
    shadow: ShadowMask,
}

impl Scene {
    fn new(chain: &FontChain) -> Self {
        let options = TextOptions { max_width: Some(70), wrap: true, ..TextOptions::default() };
        Self {
            list_runs: (layout(chain, "AB AB AB BA AB", options), layout(chain, "BBBB", TextOptions::default())),
            thumb: (0..20 * 12 * 2).map(|i| (i * 7) as u8).collect(),
            corner: CornerMask::new_in(6, Global),
            shadow: ShadowMask::new_in(4, 6, Global),
        }
    }

    fn list(&self) -> DisplayList<'_> {
        let mut list = DisplayList::new();
        list.fill(Rect::new(0, 0, 100, 80), Color::hex(0xe9eaec));
        list.fill(Rect::new(4, 4, 92, 20), Color::rgba(0, 0x80, 0xff, 128));
        list.push_clip(Rect::new(0, 8, 80, 60));
        list.text(6, 6, &self.list_runs.0, Color::hex(0x1d2126));
        list.pop_clip();
        list.text(50, 60, &self.list_runs.1, Color::rgba(200, 0, 0, 160));
        list.shadow(Rect::new(30, 30, 40, 30), self.shadow, Color::rgba(17, 20, 24, 56));
        list.round_rect(Rect::new(30, 30, 40, 30), self.corner, Color::hex(0xffffff));
        list.image(36, 40, Image::from_pixels(ImageFormat::Rgb565, 20, 12, 40, &self.thumb), Color::rgb(0, 0, 0));
        list
    }
}

#[test]
fn any_split_paints_the_same_pixels() {
    let chain = test_chain();
    let scene = Scene::new(&chain);
    let list = scene.list();
    let mut cache = GlyphCache::new(4096);
    let full = render::<Rgb565>(&list, &mut cache, 100, 80, &[Rect::new(0, 0, 100, 80)]);
    let strips: Vec<Rect> = (0..5).map(|i| Rect::new(0, i * 16, 100, 16)).collect();
    assert_eq!(render::<Rgb565>(&list, &mut cache, 100, 80, &strips), full);
    let tiles: Vec<Rect> = (0..4).flat_map(|y| (0..5).map(move |x| Rect::new(x * 20, y * 20, 20, 20))).collect();
    assert_eq!(render::<Rgb565>(&list, &mut cache, 100, 80, &tiles), full);

    let full = render::<Bgr888>(&list, &mut cache, 100, 80, &[Rect::new(0, 0, 100, 80)]);
    assert_eq!(render::<Bgr888>(&list, &mut cache, 100, 80, &strips), full);
}

fn render_half<P: Pixel>(list: &DisplayList<'_>, cache: &mut GlyphCache, area: Rect, rows: i16) -> Vec<u8> {
    let (width, stride) = (area.w as usize / 2, area.w as usize / 2 * P::BYTES);
    let mut out = vec![0u8; stride * (area.h as usize / 2)];
    let mut prepared = Prepared::new();
    prepared.prepare(list, &[area], cache);
    let mut store = ClipStore::new();
    let mut y = 0;
    while y < area.h {
        let band = Rect::new(area.x, area.y + y, area.w, rows.min(area.h - y));
        let surface = unsafe { Surface::half(out.as_mut_ptr().add(y as usize / 2 * stride), width * P::BYTES, band) };
        draw::<P, _, _, _>(list, &prepared, &mut store, &surface);
        y += rows;
    }
    out
}

#[test]
fn half_size_draws_the_pixels_at_odd_offsets() {
    fn picture<P: Pixel>() {
        let chain = test_chain();
        let scene = Scene::new(&chain);
        let alpha: Vec<u8> = (0..10 * 9).map(|i| (i * 29) as u8).collect();
        let ring = CornerMask::new_in(5, Global);
        let mut list = scene.list();
        list.border(Rect::new(3, 50, 25, 23), Some(ring), 2, Sides::ALL, Color::rgb(20, 120, 40));
        list.gradient(Rect::new(70, 5, 27, 31), Some(scene.corner), Color::rgba(255, 0, 0, 40), Color::rgb(0, 0, 255), true);
        list.arc(72, 50, ArcMask::new_in(12, 3, Global), 30.0, 250.0, Color::rgb(0, 200, 0), Color::rgb(200, 0, 200));
        list.image(5, 30, Image::from_pixels(ImageFormat::A8, 10, 9, 10, &alpha), Color::rgb(90, 0, 0));
        list.push_round_clip(Rect::new(41, 3, 27, 25), scene.corner);
        list.fill(Rect::new(0, 0, 100, 80), Color::rgba(255, 200, 0, 180));
        list.raster(Rect::new(40, 10, 30, 13), (40, 10), &Stripes);
        list.pop_round_clip();
        let mut cache = GlyphCache::new(4096);
        let full = render::<P>(&list, &mut cache, 100, 80, &[Rect::new(0, 0, 100, 80)]);
        let full_at = |x: usize, y: usize| &full[(y * 100 + x) * P::BYTES..][..P::BYTES];
        for (area, rows) in [(Rect::new(0, 0, 100, 80), 80), (Rect::new(0, 0, 100, 80), 16), (Rect::new(7, 3, 90, 75), 16)] {
            let half = render_half::<P>(&list, &mut cache, area, rows);
            let width = area.w as usize / 2;
            for v in 0..area.h as usize / 2 {
                for u in 0..width {
                    let (x, y) = (area.x as usize + u * 2 + 1, area.y as usize + v * 2 + 1);
                    assert_eq!(&half[(v * width + u) * P::BYTES..][..P::BYTES], full_at(x, y), "{area:?} {rows} at {x},{y}");
                }
            }
        }
    }
    picture::<Rgb565>();
    picture::<Bgr888>();
    picture::<Argb8888>();
}

#[test]
fn formats_agree_within_quantization() {
    let chain = test_chain();
    let scene = Scene::new(&chain);
    let list = scene.list();
    let mut cache = GlyphCache::new(4096);
    let area = [Rect::new(0, 0, 100, 80)];
    let a = render::<Rgb565>(&list, &mut cache, 100, 80, &area);
    let b = render::<Bgr888>(&list, &mut cache, 100, 80, &area);
    for y in 0..80 {
        for x in 0..100 {
            let v = px565(&a, 100, x, y);
            let at = (y * 100 + x) * 3;
            assert!(((v >> 11) as i32 * 255 / 31 - i32::from(b[at + 2])).abs() <= 16, "{x},{y} {v:04x} {:?}", &b[at..at + 3]);
            assert!((((v >> 5) & 63) as i32 * 255 / 63 - i32::from(b[at + 1])).abs() <= 12, "{x},{y}");
            assert!(((v & 31) as i32 * 255 / 31 - i32::from(b[at])).abs() <= 16, "{x},{y}");
        }
    }
}

#[test]
fn fill_clips_and_blends() {
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 4, 4), Color::rgb(0, 0, 0));
    list.fill(Rect::new(-2, 1, 4, 1), Color::rgb(255, 255, 255));
    list.fill(Rect::new(2, 2, 10, 1), Color::rgba(255, 255, 255, 128));
    let out = render::<Bgr888>(&list, &mut GlyphCache::new(64), 4, 4, &[Rect::new(0, 0, 4, 4)]);
    assert_eq!(&out[12..24], &[255, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0]);
    let row2 = &out[24..36];
    assert_eq!(&row2[..6], &[0; 6]);
    assert!((126..=130).contains(&row2[6]));
}

#[test]
fn opaque_cover_skips_earlier_ops() {
    let mut list = DisplayList::new();
    list.push_clip(Rect::new(0, 0, 4, 4));
    list.fill(Rect::new(0, 0, 4, 4), Color::rgb(255, 0, 0));
    list.pop_clip();
    list.fill(Rect::new(0, 0, 4, 4), Color::rgb(0, 255, 0));
    list.fill(Rect::new(1, 1, 1, 1), Color::rgb(0, 0, 255));
    let out = render::<Bgr888>(&list, &mut GlyphCache::new(64), 4, 4, &[Rect::new(0, 0, 4, 4)]);
    assert_eq!(&out[..3], &[0, 255, 0]);
    assert_eq!(&out[15..18], &[255, 0, 0]);
}

#[test]
fn corners_are_symmetric() {
    let corner = CornerMask::new_in(8, Global);
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 24, 24), Color::rgb(0, 0, 0));
    list.round_rect(Rect::new(0, 0, 24, 24), corner, Color::rgb(255, 255, 255));
    let out = render::<Bgr888>(&list, &mut GlyphCache::new(64), 24, 24, &[Rect::new(0, 0, 24, 24)]);
    let at = |x: usize, y: usize| out[(y * 24 + x) * 3];
    assert_eq!(at(0, 0), 0);
    assert_eq!(at(12, 12), 255);
    assert_eq!(at(12, 0), 255);
    for y in 0..24 {
        for x in 0..24 {
            assert_eq!(at(x, y), at(23 - x, y));
            assert_eq!(at(x, y), at(x, 23 - y));
            assert_eq!(at(x, y), at(y, x));
        }
    }
}

#[test]
fn rounded_border_is_hollow_and_symmetric() {
    let ring = CornerMask::ring_in(8, 2, Global);
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 24, 24), Color::rgb(0, 0, 0));
    list.border(Rect::new(0, 0, 24, 24), Some(ring), 2, Sides::ALL, Color::rgb(255, 255, 255));
    let out = render::<Bgr888>(&list, &mut GlyphCache::new(64), 24, 24, &[Rect::new(0, 0, 24, 24)]);
    let at = |x: usize, y: usize| out[(y * 24 + x) * 3];
    assert_eq!(at(0, 0), 0);
    assert_eq!(at(12, 0), 255);
    assert_eq!(at(12, 1), 255);
    assert_eq!(at(12, 2), 0);
    assert_eq!(at(12, 12), 0);
    assert_eq!(at(0, 12), 255);
    for y in 0..24 {
        for x in 0..24 {
            assert_eq!(at(x, y), at(23 - x, y));
            assert_eq!(at(x, y), at(x, 23 - y));
        }
    }
}

#[test]
fn only_the_chosen_corners_are_round() {
    let corner = CornerMask::new_in(8, Global).only(Corners::TOP);
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 24, 24), Color::rgb(0, 0, 0));
    list.round_rect(Rect::new(0, 0, 24, 24), corner, Color::rgb(255, 255, 255));
    let out = render::<Bgr888>(&list, &mut GlyphCache::new(64), 24, 24, &[Rect::new(0, 0, 24, 24)]);
    let at = |x: usize, y: usize| out[(y * 24 + x) * 3];
    assert_eq!(at(0, 0), 0);
    assert_eq!(at(23, 0), 0);
    assert_eq!(at(0, 23), 255);
    assert_eq!(at(23, 23), 255);
    assert_eq!(at(12, 0), 255);
}

#[test]
fn a_border_runs_only_along_its_sides() {
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 24, 24), Color::rgb(0, 0, 0));
    list.border(Rect::new(0, 0, 24, 24), None, 4, Sides::LEFT, Color::rgb(255, 255, 255));
    list.border(Rect::new(0, 0, 24, 24), None, 1, Sides::BOTTOM, Color::rgba(255, 255, 255, 128));
    let out = render::<Bgr888>(&list, &mut GlyphCache::new(64), 24, 24, &[Rect::new(0, 0, 24, 24)]);
    let at = |x: usize, y: usize| out[(y * 24 + x) * 3];
    assert_eq!(at(0, 0), 255);
    assert_eq!(at(3, 12), 255);
    assert_eq!(at(4, 12), 0);
    assert_eq!(at(12, 0), 0);
    assert_eq!(at(23, 12), 0);
    assert!((126..=130).contains(&at(12, 23)));
    assert!((126..=130).contains(&at(23, 23)));
}

#[test]
fn a_rounded_border_with_one_side_stops_before_the_arc() {
    let ring = CornerMask::ring_in(8, 2, Global);
    let draw = |ring: CornerMask, sides: Sides| {
        let mut list = DisplayList::new();
        list.fill(Rect::new(0, 0, 24, 24), Color::rgb(0, 0, 0));
        list.border(Rect::new(0, 0, 24, 24), Some(ring), 2, sides, Color::rgb(255, 255, 255));
        render::<Bgr888>(&list, &mut GlyphCache::new(64), 24, 24, &[Rect::new(0, 0, 24, 24)])
    };
    let out = draw(ring, Sides::TOP);
    let at = |x: usize, y: usize| out[(y * 24 + x) * 3];
    assert_eq!(at(8, 0), 255);
    assert_eq!(at(7, 0), 0);
    assert_eq!(at(0, 12), 0);
    let out = draw(ring.only(Corners::BOTTOM), Sides::TOP | Sides::LEFT);
    let at = |x: usize, y: usize| out[(y * 24 + x) * 3];
    assert_eq!(at(0, 0), 255, "a square corner meets both sides");
    assert_eq!(at(1, 1), 255);
    assert_eq!(at(2, 2), 0);
    assert_eq!(at(23, 0), 255, "a square corner carries its one side to the end");
    assert_eq!(at(23, 2), 0);
    assert_eq!(at(0, 15), 255);
    assert_eq!(at(0, 16), 0, "the rounded corner below stops the side");
}

#[test]
fn a_shadow_keeps_square_corners_square() {
    let rect = Rect::new(24, 24, 60, 50);
    let draw = |shadow: ShadowMask| {
        let mut list = DisplayList::new();
        list.fill(Rect::new(0, 0, 108, 98), Color::rgb(255, 255, 255));
        list.shadow(rect, shadow, Color::rgba(0, 0, 0, 255));
        list.fill(rect, Color::rgb(255, 255, 255));
        render::<Bgr888>(&list, &mut GlyphCache::new(64), 108, 98, &[Rect::new(0, 0, 108, 98)])
    };
    let round = draw(ShadowMask::new_in(8, 12, Global));
    let square = draw(ShadowMask::new_in(8, 0, Global));
    let mixed = draw(ShadowMask::new_in(8, 12, Global).only(Corners::TOP, ShadowMask::new_in(8, 0, Global)));
    let at = |out: &[u8], x: usize, y: usize| out[(y * 108 + x) * 3];
    for y in 0..98 {
        for x in 0..108 {
            let expected = if y < 49 { &round } else { &square };
            assert!(at(&mixed, x, y).abs_diff(at(expected, x, y)) <= 1, "{x},{y}: {} vs {}", at(&mixed, x, y), at(expected, x, y));
        }
    }
    assert!(at(&round, 25, 74) > at(&square, 25, 74), "a square corner shades where a round one leaves out");
}

#[test]
fn shadow_fades_outwards_and_stays_symmetric() {
    let shadow = ShadowMask::new_in(8, 4, Global);
    let rect = Rect::new(24, 24, 40, 30);
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 88, 78), Color::rgb(255, 255, 255));
    list.shadow(rect, shadow, Color::rgba(0, 0, 0, 255));
    let out = render::<Bgr888>(&list, &mut GlyphCache::new(64), 88, 78, &[Rect::new(0, 0, 88, 78)]);
    let at = |x: usize, y: usize| out[(y * 88 + x) * 3];
    assert_eq!(at(2, 39), 255);
    assert!(at(24, 39) < 170);
    assert!(at(16, 39) > at(20, 39));
    assert!(at(20, 39) > at(24, 39));
    for y in 0..78 {
        for x in 0..88 {
            if !rect.contains(&Rect::new(x as i16, y as i16, 1, 1)) {
                assert_eq!(at(x, y), at(87 - x, y), "{x},{y}");
                assert_eq!(at(x, y), at(x, 77 - y), "{x},{y}");
            }
        }
    }
}

#[test]
fn image_converts_to_the_target_format() {
    let bgr = [10u8, 20, 30, 40, 50, 60];
    let argb = [10u8, 20, 30, 255, 40, 50, 60, 0];
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 2, 2), Color::rgb(1, 2, 3));
    list.image(0, 0, Image::from_pixels(ImageFormat::Rgb888, 2, 1, 6, &bgr), Color::rgb(0, 0, 0));
    list.image(0, 1, Image::from_pixels(ImageFormat::Argb8888, 2, 1, 8, &argb), Color::rgb(0, 0, 0));
    let out = render::<Bgr888>(&list, &mut GlyphCache::new(64), 2, 2, &[Rect::new(0, 0, 2, 2)]);
    assert_eq!(&out, &[10, 20, 30, 40, 50, 60, 10, 20, 30, 3, 2, 1]);
}

#[test]
fn compressed_and_raw_glyphs_decode_alike() {
    let spec = [(SOLID, 8, 10, 160), (HOLLOW, 9, 6, 160)];
    let compressed = build_font(&spec, true);
    let raw = build_font(&spec, false);
    for codepoint in [SOLID, HOLLOW] {
        let a = compressed.find(codepoint as u32).unwrap();
        let b = raw.find(codepoint as u32).unwrap();
        let record = compressed.glyph(a);
        let mut left = vec![0u8; usize::from(record.width) * usize::from(record.height)];
        let mut right = left.clone();
        compressed.decode(a, &mut left);
        raw.decode(b, &mut right);
        assert_eq!(left, right);
        assert_eq!(left[0], 255);
    }
    assert!(compressed.find('Z' as u32).is_none());
}

#[test]
fn glyph_cache_keeps_a_frame_and_evicts_older_ones() {
    let glyphs: Vec<(char, u8, u8, u16)> = (0..64u32).map(|i| (char::from_u32(0x100 + i).unwrap(), 8, 8, 160)).collect();
    let font = build_font(&glyphs, true);
    let mut cache = GlyphCache::new(256);
    for pass in 0..4u32 {
        for i in 0..64 {
            cache.begin_frame();
            let index = (i + pass * 7) % 64;
            let mask = cache.get(font, index).unwrap();
            let mut expected = [0u8; 64];
            font.decode(index, &mut expected);
            assert_eq!(unsafe { core::slice::from_raw_parts(mask, 64) }, expected);
        }
    }
    let decoded = cache.decoded();
    cache.begin_frame();
    let held: Vec<_> = (0..4).map(|i| cache.get(font, i).unwrap()).collect();
    let decoded_after = cache.decoded();
    assert!(decoded_after >= decoded);
    for i in 0..4 {
        assert_eq!(cache.get(font, i), Some(held[i as usize]));
    }
    assert_eq!(cache.decoded(), decoded_after);
    let mut misses = 0;
    for i in 4..16 {
        misses += u32::from(cache.get(font, i).is_none());
    }
    assert!(misses > 0, "a frame beyond the capacity fails instead of evicting itself");
}

#[test]
fn prepare_reports_what_does_not_fit() {
    let chain = test_chain();
    let run = layout(&chain, "ABABABAB", TextOptions::default());
    let mut list = DisplayList::new();
    list.text(0, 0, &run, Color::rgb(0, 0, 0));
    let mut cache = GlyphCache::new(100);
    let mut prepared = Prepared::new();
    prepared.prepare(&list, &[Rect::new(0, 0, 200, 40)], &mut cache);
    assert_eq!(prepared.missing(), 4);
    let mut big = GlyphCache::new(1024);
    prepared.prepare(&list, &[Rect::new(0, 0, 200, 40)], &mut big);
    assert_eq!(prepared.missing(), 0);
    assert_eq!(big.decoded(), 2);
}

#[test]
fn text_wraps_at_spaces_and_wide_characters() {
    let chain = test_chain();
    let options = TextOptions { max_width: Some(30), wrap: true, ..TextOptions::default() };
    let run = layout(&chain, "AAA AA", options);
    assert_eq!(run.lines(), 2);
    assert_eq!(run.glyphs()[3].x, 0);
    assert!(run.glyphs()[3].y > run.glyphs()[0].y);
    assert_eq!(run.width(), 30);

    let wide = TextOptions { max_width: Some(34), ..options };
    let run = layout(&chain, "漢漢漢漢", wide);
    assert_eq!(run.lines(), 2);
    assert_eq!(run.glyphs()[2].x, 0);

    let run = layout(&chain, "AAAAAAAA", options);
    assert_eq!(run.lines(), 3);
    assert_eq!(run.glyphs()[3].x, 0);
}

#[test]
fn subtracting_leaves_what_is_around_a_hole() {
    let area = Rect::new(0, 0, 100, 100);
    let pieces: Vec<Rect> = area.subtract(&Rect::new(10, 20, 30, 40)).collect();
    assert_eq!(pieces, [Rect::new(0, 0, 100, 20), Rect::new(0, 20, 10, 40), Rect::new(40, 20, 60, 40), Rect::new(0, 60, 100, 40)]);
    assert_eq!(area.subtract(&Rect::new(200, 0, 10, 10)).collect::<Vec<_>>(), [area]);
    assert_eq!(area.subtract(&Rect::default()).collect::<Vec<_>>(), [area]);
    assert_eq!(area.subtract(&area).count(), 0);
}

#[test]
fn argb_keeps_transparency_and_blends_straight_alpha() {
    let mut pixels = [0u8; 8];
    let surface = unsafe { Surface::new(pixels.as_mut_ptr(), 8, Rect::new(0, 0, 2, 1)) };
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 1, 1), Color::rgba(255, 0, 0, 128));
    list.fill(Rect::new(0, 0, 1, 1), Color::rgba(255, 0, 0, 128));
    draw::<Argb8888, _, _, _>(&list, &Prepared::new(), &mut ClipStore::new(), &surface);
    assert_eq!(&pixels[4..], [0, 0, 0, 0], "untouched stays clear");
    assert_eq!(pixels[2], 255, "red does not darken over clear");
    assert!((190..=194).contains(&pixels[3]), "{}", pixels[3]);
}

#[test]
fn overlap_sees_only_what_paints_inside_after_the_mark() {
    let mut list = DisplayList::new();
    let hole = Rect::new(100, 100, 100, 100);
    list.fill(Rect::new(0, 0, 400, 400), Color::rgb(1, 1, 1));
    let first = list.ops().len();
    assert_eq!(overlap(list.ops(), first, &hole), None);
    list.push_clip(Rect::new(0, 0, 50, 50));
    list.fill(Rect::new(0, 0, 400, 400), Color::rgb(1, 1, 1));
    list.pop_clip();
    assert_eq!(overlap(list.ops(), first, &hole), None, "clipped away");
    list.fill(Rect::new(150, 150, 400, 10), Color::rgb(1, 1, 1));
    assert_eq!(overlap(list.ops(), first, &hole), Some(Rect::new(150, 150, 50, 10)));
}

#[test]
fn drawing_from_a_mark_keeps_the_clips_open_there() {
    let mut pixels = [0u8; 4 * 16];
    let surface = unsafe { Surface::new(pixels.as_mut_ptr(), 16, Rect::new(0, 0, 4, 4)) };
    let mut list = DisplayList::new();
    list.push_clip(Rect::new(0, 0, 2, 4));
    list.fill(Rect::new(0, 0, 4, 1), Color::rgb(255, 0, 0));
    let first = list.ops().len();
    list.fill(Rect::new(0, 1, 4, 1), Color::rgb(0, 255, 0));
    list.pop_clip();
    draw_from::<Argb8888, _, _, _>(&list, first, &Prepared::new(), &mut ClipStore::new(), &surface);
    assert_eq!(pixels[3], 0, "before the mark");
    assert_eq!(pixels[16 + 3], 255);
    assert_eq!(pixels[16 + 8 + 3], 0, "clipped");
}

struct Stripes;

impl Raster for Stripes {
    fn raster(&self, target: &mut RasterTarget<'_>) {
        let area = target.area();
        for y in area.y..area.bottom() {
            if y % 2 == 0 {
                target.fill(Rect::new(0, y, 100, 1), Color::rgb(255, 0, 0));
            } else {
                target.row(y).fill(0xFF);
            }
        }
    }
}

#[test]
fn a_raster_sees_its_own_coordinates_and_matches_across_strips() {
    fn picture<P: Pixel>() {
        let mut cache = GlyphCache::new(1024);
        let mut list = DisplayList::new();
        list.fill(Rect::new(0, 0, 40, 40), Color::rgb(0, 0, 0));
        list.push_clip(Rect::new(0, 0, 30, 40));
        list.raster(Rect::new(10, 5, 100, 20), (10, 5), &Stripes);
        list.pop_clip();
        let whole = render::<P>(&list, &mut cache, 40, 40, &[Rect::new(0, 0, 40, 40)]);
        let strips: Vec<Rect> = (0..4).map(|i| Rect::new(0, i * 10, 40, 10)).collect();
        assert_eq!(whole, render::<P>(&list, &mut cache, 40, 40, &strips));
        let stride = 40 * P::BYTES;
        let at = |x: usize, y: usize| &whole[y * stride + x * P::BYTES..][..P::BYTES];
        let red = render_color::<P>(Color::rgb(255, 0, 0));
        assert_eq!(at(10, 5), &red[..]);
        assert!(at(10, 6).iter().all(|&b| b == 0xFF));
        assert!(at(30, 6).iter().all(|&b| b == 0), "clipped");
        assert!(at(9, 6).iter().all(|&b| b == 0));
        assert!(at(10, 25).iter().all(|&b| b == 0));
    }
    picture::<Rgb565>();
    picture::<Bgr888>();
}

fn render_color<P: Pixel>(color: Color) -> Vec<u8> {
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 1, 1), color);
    render::<P>(&list, &mut GlyphCache::new(1024), 1, 1, &[Rect::new(0, 0, 1, 1)])
}

#[test]
fn carets_follow_wrapped_lines_and_taps_find_them() {
    let chain = test_chain();
    let options = TextOptions { max_width: Some(40), wrap: true, carets: true, ..TextOptions::default() };
    let run = layout(&chain, "AAA AAA\nA", options);
    assert_eq!(run.caret(0), Some((0, 0)));
    assert_eq!(run.caret(2), Some((20, 0)));
    assert_eq!(run.caret(4), Some((0, 14)));
    assert_eq!(run.caret(5), Some((10, 14)));
    assert_eq!(run.caret(8), Some((0, 28)));
    assert_eq!(run.caret(9), Some((10, 28)));
    assert_eq!(run.byte_at(11, 2), 1);
    assert_eq!(run.byte_at(11, 16), 5);
    assert_eq!(run.byte_at(500, 500), 9);
}

#[test]
fn text_ellipsizes_on_the_last_line() {
    let chain = test_chain();
    let options = TextOptions { max_width: Some(30), overflow: Overflow::Ellipsis, ..TextOptions::default() };
    let run = layout(&chain, "AAAAAAAA", options);
    assert_eq!(run.lines(), 1);
    assert!(run.width() <= 30);
    let last = run.glyphs()[run.glyphs().len() - 1];
    assert_eq!(usize::from(last.w), 2);

    let options = TextOptions { wrap: true, max_lines: 2, ..options };
    let run = layout(&chain, "AAAAAAAAAAAA", options);
    assert_eq!(run.lines(), 2);
}

#[test]
fn text_aligns_within_the_width() {
    let chain = test_chain();
    let center = TextOptions { max_width: Some(40), align: Align::Center, ..TextOptions::default() };
    assert_eq!(layout(&chain, "A", center).glyphs()[0].x, 15);
    let end = TextOptions { align: Align::End, ..center };
    assert_eq!(layout(&chain, "A", end).glyphs()[0].x, 30);
}

#[test]
fn decomposed_kana_compose() {
    let font = build_font(&[('か', 8, 8, 160), ('が', 9, 8, 160), ('は', 8, 8, 160), ('ぱ', 7, 8, 160)], true);
    let chain = FontChain::new(Vec::leak(vec![font]));
    let run = layout(&chain, "か\u{3099}は\u{309A}", TextOptions::default());
    assert_eq!(run.glyphs().len(), 2);
    assert_eq!(run.glyphs()[0].w, 9);
    assert_eq!(run.glyphs()[1].w, 7);
}

#[test]
fn fallback_grows_the_line_box() {
    let first = build_font(&[(SOLID, 8, 10, 160)], true);
    let second = build_font(&[(WIDE, 12, 12, 256)], true);
    let plain = FontChain::new(Vec::leak(vec![first]));
    assert_eq!(plain.line_height(), 14);
    let chained = FontChain::new(Vec::leak(vec![first, second]));
    let run = layout(&chained, "A漢", TextOptions::default());
    assert_eq!(run.glyphs().len(), 2);
    assert_eq!(run.glyphs()[1].w, 12);
}

#[test]
fn damage_merges_and_promotes() {
    let mut damage = Damage::<3>::new(Rect::new(0, 0, 100, 100));
    damage.add(Rect::new(0, 0, 10, 10));
    damage.add(Rect::new(5, 5, 10, 10));
    assert_eq!(damage.regions(), &[Rect::new(0, 0, 15, 15)]);
    damage.add(Rect::new(50, 0, 5, 5));
    damage.add(Rect::new(80, 0, 5, 5));
    damage.add(Rect::new(85, 0, 5, 5));
    assert_eq!(damage.regions().len(), 3);
    damage.add(Rect::new(200, 0, 5, 5));
    assert_eq!(damage.regions().len(), 3);
    damage.clear();
    damage.add(Rect::new(0, 0, 100, 80));
    assert_eq!(damage.regions(), &[Rect::new(0, 0, 100, 80)]);
    damage.add(Rect::new(0, 90, 100, 10));
    assert_eq!(damage.regions(), &[Rect::new(0, 0, 100, 100)]);
    damage.clear();
    assert!(damage.is_empty());
}

fn round_clip_list(corner: CornerMask) -> DisplayList<'static> {
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 32, 32), Color::rgb(255, 0, 0));
    list.fill(Rect::new(0, 0, 32, 6), Color::rgb(0, 255, 0));
    list.push_round_clip(Rect::new(4, 4, 24, 24), corner);
    list.fill(Rect::new(0, 0, 32, 32), Color::rgb(0, 0, 255));
    list.pop_round_clip();
    list
}

fn draw_rows(list: &DisplayList<'_>, rows: &[(i16, i16)]) -> Vec<u8> {
    let mut out = vec![0u8; 32 * 32 * 3];
    let mut store = ClipStore::new();
    for &(y, h) in rows {
        let surface = unsafe { Surface::new(out.as_mut_ptr().add(y as usize * 32 * 3), 32 * 3, Rect::new(0, y, 32, h)) };
        draw::<Bgr888, _, _, _>(list, &Prepared::new(), &mut store, &surface);
    }
    out
}

#[test]
fn a_round_clip_leaves_what_is_under_its_corners() {
    let out = draw_rows(&round_clip_list(CornerMask::new_in(8, Global)), &[(0, 32)]);
    let pixel = |x: usize, y: usize| [out[(y * 32 + x) * 3], out[(y * 32 + x) * 3 + 1], out[(y * 32 + x) * 3 + 2]];
    assert_eq!(pixel(4, 4), [0, 255, 0], "the row above stays in the corner");
    assert_eq!(pixel(27, 27), [0, 0, 255], "the fill under the clip stays in the corner");
    assert_eq!(pixel(16, 16), [255, 0, 0], "inside");
    assert_eq!(pixel(16, 4), [255, 0, 0], "along the top edge");
    assert_eq!(pixel(0, 0), [0, 255, 0], "outside");
}

#[test]
fn a_round_clip_cuts_only_its_round_corners() {
    let out = draw_rows(&round_clip_list(CornerMask::new_in(8, Global).only(Corners::TOP)), &[(0, 32)]);
    let pixel = |x: usize, y: usize| [out[(y * 32 + x) * 3], out[(y * 32 + x) * 3 + 1], out[(y * 32 + x) * 3 + 2]];
    assert_eq!(pixel(4, 4), [0, 255, 0], "a round corner keeps what is under it");
    assert_eq!(pixel(27, 27), [255, 0, 0], "a square corner is clipped like the rest");
    assert_eq!(pixel(4, 27), [255, 0, 0]);
}

#[test]
fn a_round_clip_matches_across_strips_cutting_its_corner() {
    let list = round_clip_list(CornerMask::new_in(8, Global));
    assert_eq!(draw_rows(&list, &[(0, 32)]), draw_rows(&list, &[(0, 7), (7, 18), (25, 7)]));
}

#[test]
fn a_round_clip_open_at_the_mark_stays_square() {
    let mut pixels = [0u8; 16 * 16 * 4];
    let surface = unsafe { Surface::new(pixels.as_mut_ptr(), 16 * 4, Rect::new(0, 0, 16, 16)) };
    let mut list = DisplayList::new();
    list.push_round_clip(Rect::new(0, 0, 16, 16), CornerMask::new_in(4, Global));
    let first = list.ops().len();
    list.fill(Rect::new(0, 0, 16, 16), Color::rgb(0, 255, 0));
    list.pop_round_clip();
    draw_from::<Argb8888, _, _, _>(&list, first, &Prepared::new(), &mut ClipStore::new(), &surface);
    assert_eq!(pixels[3], 255);
}

#[test]
fn a_gradient_goes_from_one_end_to_the_other_inside_its_corners() {
    let corner = CornerMask::new_in(4, Global);
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 40, 8), Color::rgb(0, 0, 0));
    list.gradient(Rect::new(0, 0, 40, 8), Some(corner), Color::rgba(255, 255, 255, 0), Color::rgb(255, 255, 255), false);
    let out = render::<Bgr888>(&list, &mut GlyphCache::new(64), 40, 8, &[Rect::new(0, 0, 40, 4), Rect::new(0, 4, 40, 4)]);
    let at = |x: usize, y: usize| out[(y * 40 + x) * 3];
    assert!(at(1, 4) < 10, "{}", at(1, 4));
    assert!(at(37, 4) > 235, "{}", at(37, 4));
    assert!((100..160).contains(&at(20, 4)), "{}", at(20, 4));
    assert_eq!(at(39, 0), 0, "the corner stays out");
    for x in 1..40 {
        assert!(at(x, 4) >= at(x - 1, 4));
    }
    let vertical = {
        let mut list = DisplayList::new();
        list.gradient(Rect::new(0, 0, 8, 40), None, Color::rgb(255, 255, 255), Color::rgb(0, 0, 0), true);
        render::<Bgr888>(&list, &mut GlyphCache::new(64), 8, 40, &[Rect::new(0, 0, 8, 40)])
    };
    assert!(vertical[0] > vertical[(39 * 8) * 3]);
}

#[test]
fn an_arc_covers_its_angles_with_round_ends() {
    let mask = ArcMask::new_in(12, 4, Global);
    let draw = |start: f32, sweep: f32| {
        let mut list = DisplayList::new();
        list.fill(Rect::new(0, 0, 24, 24), Color::rgb(0, 0, 0));
        list.arc(0, 0, mask, start, sweep, Color::rgb(255, 255, 255), Color::rgb(255, 255, 255));
        render::<Bgr888>(&list, &mut GlyphCache::new(64), 24, 24, &[Rect::new(0, 0, 24, 12), Rect::new(0, 12, 24, 12)])
    };
    let quarter = draw(0.0, 90.0);
    let at = |out: &[u8], x: usize, y: usize| out[(y * 24 + x) * 3];
    assert_eq!(at(&quarter, 18, 5), 255, "between 12 and 3 o'clock");
    assert_eq!(at(&quarter, 12, 12), 0, "the middle stays empty");
    assert_eq!(at(&quarter, 5, 18), 0, "past the end");
    assert!(at(&quarter, 10, 1) > 100, "the round start reaches back before 12 o'clock");
    assert!(at(&quarter, 6, 3) < 10);
    let whole = draw(45.0, 360.0);
    for (x, y) in [(12, 1), (22, 12), (12, 22), (1, 12)] {
        assert_eq!(at(&whole, x, y), 255);
    }
    for y in 0..24 {
        for x in 0..24 {
            assert_eq!(at(&whole, x, y), at(&whole, 23 - x, y));
            assert_eq!(at(&whole, x, y), at(&whole, x, 23 - y));
        }
    }
}

#[test]
fn an_arc_shades_from_its_start_to_its_end() {
    let mask = ArcMask::new_in(12, 4, Global);
    let mut list = DisplayList::new();
    list.fill(Rect::new(0, 0, 24, 24), Color::rgb(0, 0, 0));
    list.arc(0, 0, mask, 0.0, 270.0, Color::rgba(255, 255, 255, 0), Color::rgb(255, 255, 255));
    let out = render::<Rgb565>(&list, &mut GlyphCache::new(64), 24, 24, &[Rect::new(0, 0, 24, 24)]);
    let green = |x: usize, y: usize| (px565(&out, 24, x, y) >> 5) & 0x3f;
    let (right, bottom, left) = (green(22, 12), green(12, 22), green(1, 12));
    assert!(right < bottom && bottom < left, "{right} {bottom} {left}");
    assert_eq!(green(12, 1), 0, "the start is clear");
}
