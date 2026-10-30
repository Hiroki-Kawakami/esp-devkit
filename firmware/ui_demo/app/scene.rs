/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use devkit_gfx::{Align, Color, CornerMask, DisplayList, FontChain, Image, ImageFormat, Overflow, Rect, ShadowMask, TextOptions, TextRun};
use idf::heap::Caps;

use crate::resources::*;

const STATUS_H: i16 = 52;
const HEADER_H: i16 = 80;
const CRUMB_H: i16 = 48;
const LIST_Y: i16 = STATUS_H + HEADER_H + 1 + CRUMB_H;
const ROW_H: i16 = 80;
const THUMB_ROW_H: i16 = 128;
const DENSE_LINE_H: i16 = 28;
const THUMB_W: u16 = 176;
const THUMB_H: u16 = 104;
const MENU_W: i16 = 380;
const MENU_H: i16 = 420;
const SEEK_H: i16 = 128;

const SURFACE: Color = Color::hex(0xe9eaec);
const WHITE: Color = Color::hex(0xffffff);
const INK: Color = Color::hex(0x1d2126);
const MUTED: Color = Color::hex(0x78909c);
const BORDER: Color = Color::hex(0xd9dde0);
const DIVIDER: Color = Color::hex(0xeceff1);
const SLATE: Color = Color::hex(0x455a64);
const CHEVRON: Color = Color::hex(0xb0bec5);
const SHADOW: Color = Color::rgba(17, 20, 24, 56);

pub(crate) static CAPTION: FontChain = FontChain::new(&[&CAPTION_20, &JP_20]);
pub(crate) static BODY: FontChain = FontChain::new(&[&BODY_24, &JP_24]);
pub(crate) static TITLE: FontChain = FontChain::new(&[&TITLE_28]);
pub(crate) static SMALL_ICONS: FontChain = FontChain::new(&[&ICON_28]);
pub(crate) static ICONS: FontChain = FontChain::new(&[&ICON_40]);

const NAMES: [&str; 10] = [
    "Archives",
    "app-release.apk",
    "IMG_20261020_153012.jpg",
    "写真_2026年10月の旅行.jpg",
    "clip_2026-10-18.mp4",
    "議事録まとめ.txt",
    "A very long document name that does not fit on one row of the list.pdf",
    "音楽フォルダ",
    "screenshot_0925.png",
    "podcast_ep42.mp3",
];
const META: [&str; 4] = ["12 items", "48.2 MB", "2026-10-20 15:30", "3.4 GB"];
const FILE_ICONS: [char; 5] = [icons::FOLDER, icons::FILE, icons::PHOTO, icons::MOVIE, icons::MUSIC];
const MENU_ITEMS: [&str; 6] = ["Compact", "List", "Grid", "Select", "Sort", "Refresh"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    List,
    Thumbs,
    Dense,
    Menu,
    Tick,
    Seek,
}

fn text(chain: &FontChain, value: &str, options: &TextOptions) -> TextRun<Caps> {
    let mut run = TextRun::new_in(Caps::INTERNAL);
    run.layout(chain, value, options);
    run
}

fn single(chain: &FontChain, value: &str) -> TextRun<Caps> {
    text(chain, value, &TextOptions::default())
}

fn char_run(chain: &FontChain, value: char) -> TextRun<Caps> {
    let mut buffer = [0; 4];
    single(chain, value.encode_utf8(&mut buffer))
}

fn thumbnail(index: usize, format: ImageFormat) -> Vec<u8, Caps> {
    let (w, h) = (usize::from(THUMB_W), usize::from(THUMB_H));
    let mut pixels = Vec::with_capacity_in(w * h * format.bytes(), Caps::SPIRAM);
    for y in 0..h {
        for x in 0..w {
            let r = ((x * 255 / w) ^ (index * 60)) as u8;
            let g = (y * 255 / h) as u8;
            let b = ((x + y) * 4 + index * 90) as u8;
            if format == ImageFormat::Rgb888 {
                pixels.extend_from_slice(&[b, g, r]);
            } else {
                let v = u16::from(r >> 3) << 11 | u16::from(g >> 2) << 5 | u16::from(b >> 3);
                pixels.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    pixels
}

/// What a scene draws, laid out once.
pub struct Assets {
    time: TextRun<Caps>,
    percent: TextRun<Caps>,
    title: TextRun<Caps>,
    crumb: TextRun<Caps>,
    back: TextRun<Caps>,
    more: TextRun<Caps>,
    wifi: TextRun<Caps>,
    battery: TextRun<Caps>,
    chevron: TextRun<Caps>,
    names: Vec<TextRun<Caps>, Caps>,
    meta: Vec<TextRun<Caps>, Caps>,
    file_icons: Vec<TextRun<Caps>, Caps>,
    dense: Vec<TextRun<Caps>, Caps>,
    menu: Vec<TextRun<Caps>, Caps>,
    menu_title: TextRun<Caps>,
    thumbs: Vec<Vec<u8, Caps>, Caps>,
    format: ImageFormat,
    corner_12: CornerMask,
    corner_20: CornerMask,
    shadow: ShadowMask,
}

impl Assets {
    pub fn new(format: ImageFormat, width: i16) -> Self {
        let row_text = width - 88 - 64;
        let name = TextOptions { max_width: Some(row_text), overflow: Overflow::Ellipsis, ..TextOptions::default() };
        let thumb_name = TextOptions { max_width: Some(width - 228 - 64), ..name };
        let columns = (width - 24) / 3;
        let dense = TextOptions { max_width: Some(columns - 8), ..name };
        let center = TextOptions { max_width: Some(MENU_W - 48), align: Align::Center, ..TextOptions::default() };
        let mut names = Vec::new_in(Caps::INTERNAL);
        let mut dense_runs = Vec::new_in(Caps::INTERNAL);
        for value in NAMES {
            names.push(text(&BODY, value, &name));
            names.push(text(&BODY, value, &thumb_name));
            dense_runs.push(text(&CAPTION, value, &dense));
        }
        let mut meta = Vec::new_in(Caps::INTERNAL);
        for value in META {
            meta.push(single(&CAPTION, value));
        }
        let mut file_icons = Vec::new_in(Caps::INTERNAL);
        for icon in FILE_ICONS {
            file_icons.push(char_run(&ICONS, icon));
        }
        let mut menu = Vec::new_in(Caps::INTERNAL);
        for value in MENU_ITEMS {
            menu.push(single(&BODY, value));
        }
        let mut thumbs = Vec::new_in(Caps::INTERNAL);
        for index in 0..4 {
            thumbs.push(thumbnail(index, format));
        }
        Self {
            time: single(&CAPTION, "05:29"),
            percent: single(&CAPTION, "78%"),
            title: single(&TITLE, "Download"),
            crumb: single(&CAPTION, "Internal Storage › Download"),
            back: char_run(&ICONS, icons::BACK),
            more: char_run(&ICONS, icons::MORE),
            wifi: char_run(&SMALL_ICONS, icons::WIFI),
            battery: char_run(&SMALL_ICONS, icons::BATTERY),
            chevron: char_run(&SMALL_ICONS, icons::CHEVRON_RIGHT),
            names,
            meta,
            file_icons,
            dense: dense_runs,
            menu,
            menu_title: text(&CAPTION, "LAYOUT", &center),
            thumbs,
            format,
            corner_12: CornerMask::new_in(12, Caps::INTERNAL),
            corner_20: CornerMask::new_in(20, Caps::INTERNAL),
            shadow: ShadowMask::new_in(32, 20, Caps::SPIRAM),
        }
    }
}

pub struct Scene {
    pub kind: Kind,
    pub width: i16,
    pub height: i16,
    pub offset: i32,
    pub speed: i32,
}

impl Scene {
    pub fn full(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub fn damage(&self) -> Rect {
        match self.kind {
            Kind::Tick => Rect::new(28, 14, 200, 24),
            Kind::Seek => Rect::new(0, self.height - SEEK_H, self.width, SEEK_H),
            _ => Rect::new(0, LIST_Y, self.width, self.height - LIST_Y),
        }
    }

    pub fn advance(&mut self) {
        self.offset += self.speed;
    }

    pub fn build(&self, a: &'static Assets, list: &mut DisplayList<'static, Caps>) {
        list.clear();
        let w = self.width;
        list.fill(self.full(), SURFACE);
        list.fill(Rect::new(0, STATUS_H + HEADER_H, w, 1), BORDER);
        list.fill(Rect::new(0, LIST_Y - CRUMB_H, w, self.height - LIST_Y + CRUMB_H), WHITE);
        list.text(24, STATUS_H + HEADER_H + 13, &a.crumb, MUTED);

        list.push_clip(Rect::new(0, LIST_Y, w, self.height - LIST_Y));
        match self.kind {
            Kind::List | Kind::Menu | Kind::Tick | Kind::Seek => self.rows(a, list),
            Kind::Thumbs => self.thumb_rows(a, list),
            Kind::Dense => self.dense_rows(a, list),
        }
        list.pop_clip();

        if self.kind == Kind::Tick {
            let i = self.offset as usize % a.names.len();
            list.text(28, 14, &a.names[i & !1], SLATE);
        } else {
            list.text(28, 14, &a.time, SLATE);
        }
        list.text(w - 170, 12, &a.wifi, SLATE);
        list.text(w - 130, 12, &a.battery, SLATE);
        list.text(w - 96, 14, &a.percent, SLATE);
        list.text(24, STATUS_H + 20, &a.back, INK);
        list.text(84, STATUS_H + 22, &a.title, INK);
        list.text(w - 64, STATUS_H + 20, &a.more, SLATE);

        if self.kind == Kind::Menu {
            self.menu(a, list);
        }
        if self.kind == Kind::Seek {
            let y = self.height - SEEK_H;
            let filled = (self.offset % i32::from(w - 48)) as i16;
            list.fill(Rect::new(0, y, w, SEEK_H), SLATE);
            list.fill(Rect::new(24, y + 40, w - 48, 8), BORDER);
            list.fill(Rect::new(24, y + 40, filled, 8), WHITE);
            list.text(24, y + 64, &a.time, WHITE);
        }
    }

    fn rows(&self, a: &'static Assets, list: &mut DisplayList<'static, Caps>) {
        let w = self.width;
        let mut row = self.offset / i32::from(ROW_H);
        let mut y = LIST_Y - (self.offset % i32::from(ROW_H)) as i16;
        while y < self.height {
            let i = row as usize;
            list.text(24, y + 20, &a.file_icons[i % a.file_icons.len()], SLATE);
            list.text(88, y + 24, &a.names[i % NAMES.len() * 2], INK);
            list.text(w - 52, y + 26, &a.chevron, CHEVRON);
            list.fill(Rect::new(24, y + ROW_H - 1, w - 48, 1), DIVIDER);
            y += ROW_H;
            row += 1;
        }
    }

    fn thumb_rows(&self, a: &'static Assets, list: &mut DisplayList<'static, Caps>) {
        let w = self.width;
        let mut row = self.offset / i32::from(THUMB_ROW_H);
        let mut y = LIST_Y - (self.offset % i32::from(THUMB_ROW_H)) as i16;
        while y < self.height {
            let i = row as usize;
            let frame = Rect::new(24, y + 12, THUMB_W as i16, THUMB_H as i16);
            list.round_rect(frame, a.corner_12, DIVIDER);
            let pixels = &a.thumbs[i % a.thumbs.len()];
            let bytes = usize::from(THUMB_W) * a.format.bytes();
            list.image(24, y + 12, Image::from_pixels(a.format, THUMB_W, THUMB_H, bytes, pixels), Color::rgb(0, 0, 0));
            list.text(224, y + 34, &a.names[i % NAMES.len() * 2 + 1], INK);
            list.text(224, y + 70, &a.meta[i % a.meta.len()], MUTED);
            list.text(w - 52, y + 50, &a.chevron, CHEVRON);
            list.fill(Rect::new(24, y + THUMB_ROW_H - 1, w - 48, 1), DIVIDER);
            y += THUMB_ROW_H;
            row += 1;
        }
    }

    fn dense_rows(&self, a: &'static Assets, list: &mut DisplayList<'static, Caps>) {
        let w = self.width;
        let mut line = self.offset / i32::from(DENSE_LINE_H);
        let mut y = LIST_Y - (self.offset % i32::from(DENSE_LINE_H)) as i16;
        let column = (w - 24) / 3;
        while y < self.height {
            let i = line as usize;
            for c in 0..3 {
                let name = (i + c * 3) * (c + 1) % a.dense.len();
                list.text(8 + c as i16 * column, y, &a.dense[name], INK);
            }
            y += DENSE_LINE_H;
            line += 1;
        }
    }

    fn menu(&self, a: &'static Assets, list: &mut DisplayList<'static, Caps>) {
        let x = self.width - MENU_W - 16;
        let y = STATUS_H + 72;
        let panel = Rect::new(x, y, MENU_W, MENU_H);
        list.shadow(panel, a.shadow, SHADOW);
        list.round_rect(panel, a.corner_20, WHITE);
        list.text(x + 24, y + 20, &a.menu_title, MUTED);
        for (i, item) in a.menu.iter().enumerate() {
            list.text(x + 72, y + 64 + i as i16 * 56, item, INK);
        }
    }
}
