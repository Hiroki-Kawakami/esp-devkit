/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use idf::heap::Caps;

use crate::raster::{Color, Pixel, Rect, Surface};

const FIRST_CHAR: u8 = 32;
const CHARS: usize = 95;
const ICON: usize = CHARS;
const CHEVRON: usize = CHARS + 1;
const GLYPHS: usize = CHARS + 2;

const STATUS_H: i32 = 52;
const HEADER_H: i32 = 80;
const CRUMB_H: i32 = 48;
const LIST_Y: i32 = STATUS_H + HEADER_H + 1 + CRUMB_H;
const ROW_H: i32 = 80;
const DENSE_LINE_H: i32 = 28;

const SURFACE: Color = Color(0xe9, 0xea, 0xec);
const WHITE: Color = Color(0xff, 0xff, 0xff);
const INK: Color = Color(0x1d, 0x21, 0x26);
const MUTED: Color = Color(0x78, 0x90, 0x9c);
const BORDER: Color = Color(0xd9, 0xdd, 0xe0);
const DIVIDER: Color = Color(0xec, 0xef, 0xf1);
const SLATE: Color = Color(0x45, 0x5a, 0x64);

const NAMES: [&[u8]; 8] = [
    b"Archives",
    b"app-release.apk",
    b"IMG_20261020_153012.jpg",
    b"screenshot_0925.png",
    b"clip_2026-10-18.mp4",
    b"podcast_ep42.mp3",
    b"Documents and settings backup.tar.gz",
    b"notes.txt",
];

#[derive(Clone, Copy, Default)]
struct Glyph {
    offset: u32,
    w: u8,
    h: u8,
    y: i8,
    advance: u8,
}

pub struct Atlas {
    masks: Vec<u8, Caps>,
    glyphs: [Glyph; GLYPHS],
}

impl Atlas {
    pub fn new() -> Self {
        let mut masks = Vec::with_capacity_in(16 * 1024, Caps::DMA | Caps::INTERNAL);
        let mut glyphs = [Glyph::default(); GLYPHS];
        for (i, glyph) in glyphs.iter_mut().enumerate().take(CHARS) {
            let w = 9 + (i * 5 % 6) as u8;
            let tall = i % 2 == 0;
            let h = if tall { 18 } else { 13 };
            *glyph = Glyph { offset: masks.len() as u32, w, h, y: 24 - h as i8, advance: w + 2 };
            draw_char(&mut masks, i, usize::from(w), usize::from(h));
        }
        glyphs[ICON] = Glyph { offset: masks.len() as u32, w: 40, h: 40, y: 0, advance: 40 };
        draw_ring(&mut masks, 40, 40, 4);
        glyphs[CHEVRON] = Glyph { offset: masks.len() as u32, w: 28, h: 28, y: 0, advance: 28 };
        draw_ring(&mut masks, 28, 28, 3);
        Self { masks, glyphs }
    }

    fn glyph(&self, id: usize) -> (&Glyph, *const u8) {
        let g = &self.glyphs[id];
        (g, unsafe { self.masks.as_ptr().add(g.offset as usize) })
    }
}

fn draw_ring(out: &mut Vec<u8, Caps>, w: usize, h: usize, stroke: i32) {
    let r = w as i32 * 8 - 8;
    let inner = r - stroke * 16;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let dx = x * 16 + 8 - w as i32 * 8;
            let dy = y * 16 + 8 - h as i32 * 8;
            out.push(coverage(dx * dx + dy * dy, r, inner));
        }
    }
}

fn coverage(d2: i32, outer: i32, inner: i32) -> u8 {
    let o2 = outer * outer;
    let i2 = inner * inner;
    let edge = outer * 16;
    if d2 >= o2 + edge || d2 <= i2 - edge {
        0
    } else if d2 > o2 - edge || d2 < i2 + edge {
        110
    } else {
        255
    }
}

fn draw_char(out: &mut Vec<u8, Caps>, id: usize, w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let left = x < 3;
            let right = x + 3 >= w;
            let top = y < 3;
            let mid = y >= h / 2 - 1 && y < h / 2 + 2;
            let bottom = y + 3 >= h;
            let on = match id % 5 {
                0 => left || right || top || bottom,
                1 => left || right || mid,
                2 => left || top || mid,
                3 => top || bottom || mid,
                _ => left || bottom,
            };
            let edge = x == 0 || x + 1 == w || y == 0 || y + 1 == h;
            out.push(match (on, edge) {
                (false, _) => 0,
                (true, true) => 96,
                (true, false) => 255,
            });
        }
    }
}

#[derive(Clone, Copy)]
pub enum Op {
    Fill(Rect, Color),
    Text { x: i32, y: i32, text: &'static [u8], color: Color },
    Mask { x: i32, y: i32, id: usize, color: Color },
}

impl Op {
    fn top_bottom(&self) -> (i32, i32) {
        match self {
            Self::Fill(r, _) => (r.y, r.y + r.h),
            Self::Text { y, .. } => (*y, *y + 24),
            Self::Mask { y, .. } => (*y, *y + 40),
        }
    }

    fn draw<P: Pixel>(&self, atlas: &Atlas, s: &Surface) {
        match *self {
            Self::Fill(r, c) => s.fill::<P>(&r, c),
            Self::Mask { x, y, id, color } => {
                let (g, mask) = atlas.glyph(id);
                s.mask::<P>(&Rect::new(x, y, i32::from(g.w), i32::from(g.h)), mask, color);
            }
            Self::Text { mut x, y, text, color } => {
                let right = s.area.x + s.area.w;
                for &ch in text {
                    if x >= right {
                        break;
                    }
                    let (g, mask) = atlas.glyph(usize::from(ch.wrapping_sub(FIRST_CHAR)) % CHARS);
                    let rect = Rect::new(x, y + i32::from(g.y), i32::from(g.w), i32::from(g.h));
                    s.mask::<P>(&rect, mask, color);
                    x += i32::from(g.advance);
                }
            }
        }
    }
}

pub fn draw<P: Pixel>(ops: &[Op], atlas: &Atlas, s: &Surface) {
    let (top, bottom) = (s.area.y, s.area.y + s.area.h);
    for op in ops {
        let (t, b) = op.top_bottom();
        if t < bottom && b > top {
            op.draw::<P>(atlas, s);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    List,
    Dense,
    Tick,
}

pub struct Scene {
    pub kind: Kind,
    pub width: i32,
    pub height: i32,
    pub offset: i32,
    pub speed: i32,
}

impl Scene {
    pub fn full(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub fn damage(&self) -> Rect {
        match self.kind {
            Kind::List | Kind::Dense => Rect::new(0, LIST_Y, self.width, self.height - LIST_Y),
            Kind::Tick => Rect::new(28, 14, 200, 24),
        }
    }

    pub fn advance(&mut self) {
        self.offset += self.speed;
    }

    pub fn build(&self, ops: &mut Vec<Op, Caps>) {
        ops.clear();
        let w = self.width;
        match self.kind {
            Kind::List | Kind::Tick => self.build_list(ops),
            Kind::Dense => self.build_dense(ops),
        }
        ops.push(Op::Fill(Rect::new(0, 0, w, STATUS_H + HEADER_H), SURFACE));
        let clock = if self.kind == Kind::Tick { NAMES[(self.offset as usize) % NAMES.len()] } else { b"05:29" };
        ops.push(Op::Text { x: 28, y: 14, text: clock, color: SLATE });
        ops.push(Op::Mask { x: w - 120, y: 12, id: CHEVRON, color: SLATE });
        ops.push(Op::Mask { x: 24, y: STATUS_H + 20, id: ICON, color: INK });
        ops.push(Op::Text { x: 88, y: STATUS_H + 28, text: b"Download", color: INK });
        ops.push(Op::Fill(Rect::new(0, STATUS_H + HEADER_H, w, 1), BORDER));
        ops.push(Op::Fill(Rect::new(0, STATUS_H + HEADER_H + 1, w, CRUMB_H), WHITE));
        ops.push(Op::Text { x: 24, y: STATUS_H + HEADER_H + 13, text: b"Internal Storage / Download", color: MUTED });
    }

    fn build_list(&self, ops: &mut Vec<Op, Caps>) {
        let w = self.width;
        ops.push(Op::Fill(Rect::new(0, LIST_Y, w, self.height - LIST_Y), WHITE));
        let first = self.offset / ROW_H;
        let mut y = LIST_Y - self.offset % ROW_H;
        let mut row = first;
        while y < self.height {
            let name = NAMES[row as usize % NAMES.len()];
            ops.push(Op::Mask { x: 24, y: y + 20, id: ICON, color: SLATE });
            ops.push(Op::Text { x: 88, y: y + 28, text: name, color: INK });
            ops.push(Op::Mask { x: w - 52, y: y + 26, id: CHEVRON, color: MUTED });
            ops.push(Op::Fill(Rect::new(24, y + ROW_H - 1, w - 48, 1), DIVIDER));
            y += ROW_H;
            row += 1;
        }
    }

    fn build_dense(&self, ops: &mut Vec<Op, Caps>) {
        let w = self.width;
        ops.push(Op::Fill(Rect::new(0, LIST_Y, w, self.height - LIST_Y), WHITE));
        let mut y = LIST_Y - self.offset % DENSE_LINE_H;
        let mut line = self.offset / DENSE_LINE_H;
        while y < self.height {
            let a = NAMES[line as usize % NAMES.len()];
            let b = NAMES[(line as usize * 3 + 1) % NAMES.len()];
            let c = NAMES[(line as usize * 5 + 2) % NAMES.len()];
            ops.push(Op::Text { x: 8, y, text: a, color: INK });
            ops.push(Op::Text { x: 8 + w / 3, y, text: b, color: INK });
            ops.push(Op::Text { x: 8 + w * 2 / 3, y, text: c, color: INK });
            y += DENSE_LINE_H;
            line += 1;
        }
    }
}
