/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec;
use alloc::vec::Vec;

use devkit_gfx::{Color, Image, ImageFormat, Rect, Rotation, Size, Target};
use idf::Error;

use super::{Alpha, Blend, Fill, Slot, Srm, ppa};

fn words(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn read(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

fn srm(src: &[u16], w: u16, target: &mut Target<'_>, set: impl FnOnce(&mut Srm<'_>)) -> idf::Result<()> {
    let pixels = words(src);
    let image = Image::from_pixels(ImageFormat::Rgb565, w, (src.len() / usize::from(w)) as u16, usize::from(w), &pixels);
    let mut op = Srm::new(image);
    set(&mut op);
    ppa()?.srm(&op, target, &mut Slot::new())?.wait()
}

#[test]
fn srm_turns_counter_clockwise() {
    let mut out = vec![0u8; 4];
    let mut target = Target::new(&mut out, ImageFormat::Rgb565, Size::new(1, 2), 1).unwrap();
    srm(&[0x1234, 0xabcd], 2, &mut target, |op| op.rotation = Rotation::R90).unwrap();
    assert_eq!(read(&out), [0xabcd, 0x1234]);
}

#[test]
fn a_turned_target_turns_what_lands_on_it() {
    let mut out = vec![0u8; 4];
    let target = Target::new(&mut out, ImageFormat::Rgb565, Size::new(1, 2), 1).unwrap();
    let mut target = target.rotated(Rotation::R90);
    srm(&[0x1234, 0xabcd], 2, &mut target, |_| {}).unwrap();
    assert_eq!(read(&out), [0xabcd, 0x1234]);
}

#[test]
fn mirrors_stay_on_the_logical_axes_of_a_turned_target() {
    let src = [0x1111, 0x2222, 0x3333];
    for (mirror_x, stored) in [(false, [0x3333, 0x2222, 0x1111]), (true, [0x1111, 0x2222, 0x3333])] {
        let mut out = vec![0u8; 6];
        let target = Target::new(&mut out, ImageFormat::Rgb565, Size::new(1, 3), 1).unwrap();
        let mut target = target.rotated(Rotation::R90);
        srm(&src, 3, &mut target, |op| op.mirror_x = mirror_x).unwrap();
        assert_eq!(read(&out), stored);
    }
}

#[test]
fn srm_lands_at_its_place_and_scales() {
    let mut out = vec![0u8; 4 * 4 * 2];
    let mut target = Target::new(&mut out, ImageFormat::Rgb565, Size::new(4, 4), 4).unwrap();
    srm(&[0xffff], 1, &mut target, |op| {
        op.at = (1, 2);
        op.scale_x = 2.0;
        op.scale_y = 2.0;
    })
    .unwrap();
    let px = read(&out);
    for y in 0..4 {
        for x in 0..4 {
            let inside = (1..3).contains(&x) && (2..4).contains(&y);
            assert_eq!(px[y * 4 + x], if inside { 0xffff } else { 0 }, "({x}, {y})");
        }
    }
}

#[test]
fn what_the_engine_cannot_do_is_refused() {
    let mut out = vec![0u8; 8];
    let mut target = Target::new(&mut out, ImageFormat::Rgb565, Size::new(2, 2), 2).unwrap();
    assert_eq!(srm(&[1, 2], 2, &mut target, |op| op.scale_x = 0.0).err(), Some(Error::INVALID_ARG));
    assert_eq!(srm(&[1, 2], 2, &mut target, |op| op.src_rect = Rect::new(1, 0, 2, 1)).err(), Some(Error::INVALID_ARG));
    assert_eq!(srm(&[1, 2], 2, &mut target, |op| op.byte_swap = true).err(), None);

    let mut gray = vec![0u8; 4];
    let mut l4 = Target::new(&mut gray, ImageFormat::L4, Size::new(2, 2), 2).unwrap();
    assert_eq!(srm(&[1, 2], 2, &mut l4, |_| {}).err(), Some(Error::NOT_SUPPORTED));
}

#[test]
fn what_leaves_the_area_is_cut_off() {
    let mut out = vec![0u8; 8];
    let mut target = Target::new(&mut out, ImageFormat::Rgb565, Size::new(2, 2), 2).unwrap();
    srm(&[1, 2], 2, &mut target, |op| op.at = (1, 0)).unwrap();
    srm(&[3, 4], 2, &mut target, |op| op.at = (-5, 1)).unwrap();
    assert_eq!(read(&out), [0, 1, 0, 0]);
}

/// A cut job writes what the whole one writes inside the area, and nothing
/// outside it, however it is turned and mirrored.
#[test]
fn a_cut_job_matches_the_whole_one_inside_the_area() {
    let src: Vec<u16> = (1..=6 * 4).collect();
    let area = Rect::new(2, 1, 4, 3);
    for rotation in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
        for (mirror_x, mirror_y) in [(false, false), (true, false), (false, true)] {
            for target_turn in [Rotation::R0, Rotation::R90] {
                let set = |op: &mut Srm<'_>| {
                    op.at = (1, -1);
                    op.rotation = rotation;
                    op.mirror_x = mirror_x;
                    op.mirror_y = mirror_y;
                };
                let mut whole = vec![0u8; 8 * 8 * 2];
                let t = Target::new(&mut whole, ImageFormat::Rgb565, Size::new(8, 8), 8).unwrap();
                srm(&src, 6, &mut t.rotated(target_turn), set).unwrap();
                let mut cut = vec![0u8; 8 * 8 * 2];
                let t = Target::new(&mut cut, ImageFormat::Rgb565, Size::new(8, 8), 8).unwrap();
                srm(&src, 6, &mut t.rotated(target_turn).crop(area).unwrap(), set).unwrap();
                let (whole, cut) = (read(&whole), read(&cut));
                let mut probe = [0u8; 128];
                let probe = Target::new(&mut probe, ImageFormat::Rgb565, Size::new(8, 8), 8).unwrap().rotated(target_turn);
                let stored = probe.stored_rect(area).unwrap();
                for y in 0..8 {
                    for x in 0..8 {
                        let inside = Rect::new(x, y, 1, 1);
                        let expect = if stored.contains(&inside) { whole[(y * 8 + x) as usize] } else { 0 };
                        assert_eq!(cut[(y * 8 + x) as usize], expect, "{rotation:?} {mirror_x} {mirror_y} {target_turn:?} ({x}, {y})");
                    }
                }
            }
        }
    }
}

#[test]
fn fill_covers_its_rect_on_a_turned_target() {
    let mut out = vec![0u8; 2 * 3 * 2];
    let target = Target::new(&mut out, ImageFormat::Rgb565, Size::new(2, 3), 2).unwrap();
    let mut target = target.rotated(Rotation::R90);
    let op = Fill { rect: Rect::new(0, 0, 1, 2), color: Color::rgb(255, 255, 255) };
    ppa().unwrap().fill(&op, &mut target, &mut Slot::new()).unwrap().wait().unwrap();
    assert_eq!(read(&out), [0, 0, 0, 0, 0xffff, 0xffff]);
}

#[test]
fn blend_lays_an_alpha_mask_over_the_target() {
    let mut out = vec![0u8; 2 * 3];
    let mut target = Target::new(&mut out, ImageFormat::Bgr888, Size::new(2, 1), 2).unwrap();
    let mask = [255u8, 0];
    let fg = Image::from_pixels(ImageFormat::A8, 2, 1, 2, &mask);
    let mut op = Blend::new(fg, (0, 0));
    op.fg_color = Color::rgb(10, 20, 30);
    op.bg_alpha = Alpha::Fixed(255);
    ppa().unwrap().blend(&op, &mut target, &mut Slot::new()).unwrap().wait().unwrap();
    assert_eq!(out, [30, 20, 10, 0, 0, 0]);

    let mut turned = Target::new(&mut out, ImageFormat::Bgr888, Size::new(2, 1), 2).unwrap().rotated(Rotation::R180);
    assert_eq!(ppa().unwrap().blend(&op, &mut turned, &mut Slot::new()).err(), Some(Error::NOT_SUPPORTED));
}
