/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::alloc::Global;
use alloc::vec::Vec;
use core::alloc::Allocator;

use crate::font::Font;
use crate::format::GlyphRecord;
use crate::geom::Rect;

const VOICED_MARK: u32 = 0x3099;
const SEMI_VOICED_MARK: u32 = 0x309A;
const FIRST_WIDE: u32 = 0x2E80;

const VOICED: [(u32, u32); 48] = [
    (0x3046, 0x3094), (0x304B, 0x304C), (0x304D, 0x304E), (0x304F, 0x3050), (0x3051, 0x3052), (0x3053, 0x3054),
    (0x3055, 0x3056), (0x3057, 0x3058), (0x3059, 0x305A), (0x305B, 0x305C), (0x305D, 0x305E), (0x305F, 0x3060),
    (0x3061, 0x3062), (0x3064, 0x3065), (0x3066, 0x3067), (0x3068, 0x3069), (0x306F, 0x3070), (0x3072, 0x3073),
    (0x3075, 0x3076), (0x3078, 0x3079), (0x307B, 0x307C), (0x309D, 0x309E), (0x30A6, 0x30F4), (0x30AB, 0x30AC),
    (0x30AD, 0x30AE), (0x30AF, 0x30B0), (0x30B1, 0x30B2), (0x30B3, 0x30B4), (0x30B5, 0x30B6), (0x30B7, 0x30B8),
    (0x30B9, 0x30BA), (0x30BB, 0x30BC), (0x30BD, 0x30BE), (0x30BF, 0x30C0), (0x30C1, 0x30C2), (0x30C4, 0x30C5),
    (0x30C6, 0x30C7), (0x30C8, 0x30C9), (0x30CF, 0x30D0), (0x30D2, 0x30D3), (0x30D5, 0x30D6), (0x30D8, 0x30D9),
    (0x30DB, 0x30DC), (0x30EF, 0x30F7), (0x30F0, 0x30F8), (0x30F1, 0x30F9), (0x30F2, 0x30FA), (0x30FD, 0x30FE),
];

const SEMI_VOICED: [(u32, u32); 10] = [
    (0x306F, 0x3071), (0x3072, 0x3074), (0x3075, 0x3077), (0x3078, 0x307A), (0x307B, 0x307D), (0x30CF, 0x30D1),
    (0x30D2, 0x30D4), (0x30D5, 0x30D7), (0x30D8, 0x30DA), (0x30DB, 0x30DD),
];

fn compose(base: u32, mark: u32) -> u32 {
    let table: &[(u32, u32)] = if mark == VOICED_MARK { &VOICED } else { &SEMI_VOICED };
    table.iter().find(|(b, _)| *b == base).map_or(base, |(_, composed)| *composed)
}

/// Fonts tried in order, for scripts or icons the first one lacks. The line box
/// is the first font's, grown only as far as the others' glyphs need.
pub struct FontChain {
    fonts: &'static [&'static Font],
    ascent: i16,
    descent: i16,
}

impl FontChain {
    pub const fn new(fonts: &'static [&'static Font]) -> Self {
        let mut ascent = fonts[0].line_height() - fonts[0].base_line();
        let mut descent = fonts[0].base_line();
        let mut i = 1;
        while i < fonts.len() {
            if fonts[i].max_ascent() > ascent {
                ascent = fonts[i].max_ascent();
            }
            if fonts[i].max_descent() > descent {
                descent = fonts[i].max_descent();
            }
            i += 1;
        }
        Self { fonts, ascent, descent }
    }

    pub const fn line_height(&self) -> i16 {
        self.ascent + self.descent
    }

    fn resolve(&self, codepoint: u32) -> Option<(&'static Font, u32, GlyphRecord)> {
        self.fonts.iter().find_map(|font| {
            let index = font.find(codepoint)?;
            Some((*font, index, font.glyph(index)))
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overflow {
    Clip,
    Ellipsis,
}

#[derive(Clone, Copy)]
pub struct TextOptions {
    /// `Center` and `End` align within this width.
    pub max_width: Option<i16>,
    pub wrap: bool,
    /// 0 is unlimited.
    pub max_lines: u16,
    pub overflow: Overflow,
    pub align: Align,
    /// `None` is the chain's.
    pub line_height: Option<i16>,
    /// Records where each character starts, for `TextRun::caret`.
    pub carets: bool,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self { max_width: None, wrap: false, max_lines: 0, overflow: Overflow::Clip, align: Align::Start, line_height: None, carets: false }
    }
}

#[derive(Clone, Copy)]
pub struct PlacedGlyph {
    pub font: &'static Font,
    pub glyph: u32,
    pub x: i16,
    pub y: i16,
    pub w: u8,
    pub h: u8,
}

/// Laid out text, positioned relative to its top left corner. Keep one per
/// string: moving it costs nothing, laying it out again does.
pub struct TextRun<A: Allocator = Global> {
    glyphs: Vec<PlacedGlyph, A>,
    carets: Vec<Caret, A>,
    line_height: i16,
    width: i16,
    height: i16,
    lines: u16,
    bounds: Rect,
}

impl TextRun<Global> {
    pub fn new() -> Self {
        Self::new_in(Global)
    }
}

impl Default for TextRun<Global> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
struct Caret {
    byte: u32,
    x: i16,
    line: u16,
}

struct Line {
    start: usize,
    caret_start: usize,
    break_caret: usize,
    splitting: bool,
    pen: i32,
    ink: i32,
    break_glyph: usize,
    break_pen: i32,
    break_ink: i32,
    number: u16,
    top: i16,
}

#[inline]
fn px(pen: i32) -> i16 {
    ((pen + 8) >> 4) as i16
}

impl<A: Allocator + Clone> TextRun<A> {
    pub fn new_in(alloc: A) -> Self {
        Self { glyphs: Vec::new_in(alloc.clone()), carets: Vec::new_in(alloc), line_height: 0, width: 0, height: 0, lines: 0, bounds: Rect::default() }
    }
}

impl<A: Allocator> TextRun<A> {
    pub fn glyphs(&self) -> &[PlacedGlyph] {
        &self.glyphs
    }

    /// Where the character at `byte` starts, its line's top left; needs
    /// `TextOptions::carets`.
    pub fn caret(&self, byte: usize) -> Option<(i16, i16)> {
        let found = self.carets.iter().rev().find(|c| c.byte as usize <= byte)?;
        Some((found.x, found.line as i16 * self.line_height))
    }

    /// The character boundary nearest to a point, as a byte offset.
    pub fn byte_at(&self, x: i16, y: i16) -> usize {
        let line = (y.max(0) / self.line_height.max(1)) as u16;
        let last = self.carets.iter().map(|c| c.line).max().unwrap_or(0);
        let line = line.min(last);
        let mut best = (i16::MAX, 0);
        for c in self.carets.iter().filter(|c| c.line == line) {
            let distance = (c.x - x).abs();
            if distance < best.0 {
                best = (distance, c.byte as usize);
            }
        }
        best.1
    }

    pub const fn width(&self) -> i16 {
        self.width
    }

    pub const fn height(&self) -> i16 {
        self.height
    }

    pub const fn lines(&self) -> u16 {
        self.lines
    }

    /// The ink relative to the run's origin.
    pub const fn bounds(&self) -> Rect {
        self.bounds
    }

    fn place(&mut self, line: &mut Line, font: &'static Font, glyph: u32, record: &GlyphRecord) {
        if record.width != 0 {
            self.glyphs.push(PlacedGlyph {
                font,
                glyph,
                x: px(line.pen) + i16::from(record.ofs_x),
                y: -(i16::from(record.height) + i16::from(record.ofs_y)),
                w: record.width,
                h: record.height,
            });
        }
        line.pen += i32::from(record.advance);
    }

    /// Turns the glyphs from `line.start` to `end` into a finished line of
    /// `ink` 1/16 px and starts the next one with the rest.
    fn finish(&mut self, line: &mut Line, end: usize, ink: i32, chain: &FontChain, options: &TextOptions, line_height: i16) {
        let width = px(ink);
        let shift = match (options.align, options.max_width) {
            (Align::Center, Some(max)) => (max - width) / 2,
            (Align::End, Some(max)) => max - width,
            _ => 0,
        };
        let baseline = line.top + chain.ascent + (line_height - chain.line_height()) / 2;
        for g in &mut self.glyphs[line.start..end] {
            g.x += shift;
            g.y += baseline;
        }
        let caret_end = if end == self.glyphs.len() && !line.splitting { self.carets.len() } else { line.break_caret };
        let number = line.number;
        for c in &mut self.carets[line.caret_start..caret_end] {
            c.x += shift;
            c.line = number;
        }
        line.caret_start = caret_end;
        self.width = self.width.max(width);
        line.start = end;
        line.top += line_height;
        line.number += 1;
    }

    fn start_next(&mut self, line: &mut Line, from_pen: i32) {
        let shift = px(from_pen);
        for g in &mut self.glyphs[line.start..] {
            g.x -= shift;
        }
        for c in &mut self.carets[line.caret_start..] {
            c.x -= shift;
        }
        line.pen -= from_pen;
        line.ink = (line.ink - from_pen).max(0);
        line.break_glyph = line.start;
        line.break_pen = 0;
        line.break_ink = 0;
    }

    fn ellipsize(&mut self, line: &mut Line, chain: &FontChain, limit: i32) {
        let mark = chain.resolve('…' as u32);
        let dot = chain.resolve('.' as u32);
        let (glyphs, width) = match (&mark, &dot) {
            (Some(m), _) => (core::slice::from_ref(m), i32::from(m.2.advance)),
            (None, Some(d)) => (core::slice::from_ref(d), i32::from(d.2.advance) * 3),
            (None, None) => return,
        };
        let repeat = if mark.is_some() { 1 } else { 3 };
        while self.glyphs.len() > line.start {
            let last = self.glyphs[self.glyphs.len() - 1];
            let end = (i32::from(last.x) + i32::from(last.w)) * 16;
            if end + width <= limit {
                line.pen = end + 16;
                break;
            }
            self.glyphs.pop();
            line.pen = i32::from(last.x) * 16;
        }
        for _ in 0..repeat {
            let (font, glyph, record) = &glyphs[0];
            self.place(line, font, *glyph, record);
        }
        line.ink = line.pen;
    }

    pub fn layout(&mut self, chain: &FontChain, text: &str, options: &TextOptions) {
        self.glyphs.clear();
        self.carets.clear();
        self.width = 0;
        let line_height = options.line_height.unwrap_or(chain.line_height());
        self.line_height = line_height;
        let limit = options.max_width.map_or(i32::MAX, |w| i32::from(w) * 16);
        let mut line = Line { start: 0, caret_start: 0, break_caret: 0, splitting: false, pen: 0, ink: 0, break_glyph: 0, break_pen: 0, break_ink: 0, number: 0, top: 0 };
        let mut chars = text.char_indices().peekable();
        while let Some((byte, c)) = chars.next() {
            let mut codepoint = c as u32;
            if codepoint == VOICED_MARK || codepoint == SEMI_VOICED_MARK {
                continue;
            }
            if options.carets {
                self.carets.push(Caret { byte: byte as u32, x: px(line.pen), line: 0 });
            }
            if c == '\n' {
                let ink = line.ink;
                let end = self.glyphs.len();
                line.break_caret = self.carets.len();
                line.splitting = true;
                self.finish(&mut line, end, ink, chain, options, line_height);
                line.splitting = false;
                line.pen = 0;
                self.start_next(&mut line, 0);
                line.ink = 0;
                continue;
            }
            if let Some(&(_, next)) = chars.peek()
                && matches!(next as u32, VOICED_MARK | SEMI_VOICED_MARK)
            {
                codepoint = compose(codepoint, next as u32);
            }
            let Some((font, glyph, record)) = chain.resolve(codepoint) else { continue };
            let space = c == ' ';
            if !space && codepoint >= FIRST_WIDE && line.pen > 0 {
                line.break_caret = self.carets.len().saturating_sub(1);
                line.break_glyph = self.glyphs.len();
                line.break_pen = line.pen;
                line.break_ink = line.ink;
            }
            if !space && line.pen + i32::from(record.advance) > limit {
                let last_line = options.max_lines != 0 && line.number + 1 >= options.max_lines;
                if options.wrap && !last_line {
                    let (end, from, ink) = if line.break_pen > 0 {
                        (line.break_glyph, line.break_pen, line.break_ink)
                    } else {
                        line.break_caret = self.carets.len().saturating_sub(1);
                        (self.glyphs.len(), line.pen, line.ink)
                    };
                    line.splitting = true;
                    self.finish(&mut line, end, ink, chain, options, line_height);
                    line.splitting = false;
                    self.start_next(&mut line, from);
                } else if options.overflow == Overflow::Ellipsis {
                    self.ellipsize(&mut line, chain, limit);
                    break;
                } else if options.wrap {
                    break;
                }
            }
            self.place(&mut line, font, glyph, &record);
            if space {
                line.break_caret = self.carets.len();
                line.break_glyph = self.glyphs.len();
                line.break_pen = line.pen;
                line.break_ink = line.ink;
            } else {
                line.ink = line.pen;
            }
        }
        if options.carets {
            self.carets.push(Caret { byte: text.len() as u32, x: px(line.pen), line: 0 });
        }
        let (end, ink) = (self.glyphs.len(), line.ink);
        self.finish(&mut line, end, ink, chain, options, line_height);
        self.lines = line.number;
        self.height = line.top;
        self.bounds = self.glyphs.iter().fold(Rect::default(), |bounds, g| {
            bounds.union(&Rect::new(g.x, g.y, i16::from(g.w), i16::from(g.h)))
        });
    }
}
