/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::vec::Vec;

use devkit_gfx::Color;
use devkit_ui::{Align, Child, Component, Cx, Element, ListId, Role, Shadow, Ui};

use crate::resources::icons;
use crate::scene::{BODY, CAPTION, ICONS, SMALL_ICONS, TITLE};

const FILES: ListId = 0;
const FILE_COUNT: usize = 500;
const ROW_IDS: [&str; 8] = ["row0", "row1", "row2", "row3", "row4", "row5", "row6", "row7"];

const SURFACE: Color = Color::hex(0xe9eaec);
const WHITE: Color = Color::hex(0xffffff);
const INK: Color = Color::hex(0x1d2126);
const MUTED: Color = Color::hex(0x78909c);
const BORDER: Color = Color::hex(0xd9dde0);
const DIVIDER: Color = Color::hex(0xeceff1);
const SLATE: Color = Color::hex(0x455a64);
const CHEVRON: Color = Color::hex(0xb0bec5);
const PRESSED: Color = Color::hex(0xdfe3e6);
const SELECTED: Color = Color::hex(0xe3f2fd);
const BLUE: Color = Color::hex(0x2196f3);
const MENU_SHADOW: Shadow = Shadow { y: 12, blur: 32, color: Color::rgba(17, 20, 24, 56) };

const NAMES: [&str; 8] = [
    "Archives",
    "app-release.apk",
    "IMG_20261020_153012.jpg",
    "写真_2026年10月の旅行.jpg",
    "clip_2026-10-18.mp4",
    "議事録まとめ.txt",
    "A very long document name that does not fit on one row of the list.pdf",
    "音楽フォルダ",
];
const FILE_ICONS: [char; 5] = [icons::FOLDER, icons::FILE, icons::PHOTO, icons::MOVIE, icons::MUSIC];

#[derive(Clone, Copy)]
pub enum Msg {
    Tick,
    Open(u16),
    ToggleMenu,
    SortChanged(bool),
}

pub struct FileBrowser {
    names: Vec<String>,
    selected: Option<u16>,
    menu: bool,
    seconds: u32,
    sorted: bool,
    revision: u32,
}

impl FileBrowser {
    pub fn new() -> Self {
        let names = (0..FILE_COUNT).map(|i| alloc::format!("{:03} {}", i, NAMES[i % NAMES.len()])).collect();
        Self { names, selected: None, menu: false, seconds: 5 * 3600 + 29 * 60, sorted: false, revision: 0 }
    }

    fn status_bar<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        let time = ui.format(format_args!("{:02}:{:02}:{:02}", self.seconds / 3600, self.seconds / 60 % 60, self.seconds % 60));
        ui.row([
            ui.text(&CAPTION, time).color(SLATE).id("clock").grow(1),
            ui.icon(&SMALL_ICONS, icons::WIFI).color(SLATE),
            ui.icon(&SMALL_ICONS, icons::BATTERY).color(SLATE),
            ui.text(&CAPTION, "78%").color(SLATE),
        ])
        .height(52)
        .padding_x(28)
        .gap(14)
        .align(Align::Center)
    }

    fn header<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        ui.row([
            ui.row([ui.icon(&ICONS, icons::BACK).color(INK)]).size(64, 64).padding(12).radius(32).pressed_background(PRESSED),
            ui.text(&TITLE, if self.sorted { "Download (sorted)" } else { "Download" }).color(INK).id("title").ellipsis().grow(1),
            ui.row([ui.icon(&ICONS, icons::MORE).color(SLATE)])
                .size(64, 64)
                .padding(12)
                .radius(32)
                .pressed_background(PRESSED)
                .role(Role::Button)
                .id("more")
                .on_tap(Msg::ToggleMenu),
        ])
        .height(80)
        .padding_x(12)
        .gap(8)
        .align(Align::Center)
    }

    fn menu<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        if !self.menu {
            return ui.empty();
        }
        ui.column([
            ui.column([]).absolute(Some(0), Some(0), Some(0), Some(0)).id("backdrop").on_tap(Msg::ToggleMenu),
            ui.column([
                ui.text(&CAPTION, "LAYOUT").color(MUTED),
                ui.child_mapped::<SortOption>(self.sorted, Msg::SortChanged).id("sort"),
                ui.text(&BODY, "Refresh").color(INK),
            ])
            .width(380)
            .padding(24)
            .gap(20)
            .radius(20)
            .background(WHITE)
            .shadow(MENU_SHADOW)
            .absolute(None, Some(124), Some(16), None)
            .id("menu"),
        ])
        .absolute(Some(0), Some(0), Some(0), Some(0))
    }
}

impl Component for FileBrowser {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, cx: &mut Cx<Self>) {
        match msg {
            Msg::Tick => self.seconds += 1,
            Msg::Open(i) => {
                self.selected = Some(i);
                self.revision += 1;
            }
            Msg::ToggleMenu => self.menu = !self.menu,
            Msg::SortChanged(sorted) => self.sorted = sorted,
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        ui.column([
            self.status_bar(ui),
            self.header(ui),
            ui.text(&CAPTION, "Internal Storage › Download")
                .color(MUTED)
                .height(48)
                .padding_each(24, 13, 24, 0)
                .border_top(BORDER),
            ui.list(FILES, self.names.len(), 80).revision(self.revision).id("files").grow(1).background(WHITE),
            self.menu(ui),
        ])
        .background(SURFACE)
    }

    fn list_row<'a>(&'a self, _list: ListId, index: usize, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        let selected = self.selected == Some(index as u16);
        let row = ui
            .row([
                ui.icon(&ICONS, FILE_ICONS[index % FILE_ICONS.len()]).color(SLATE),
                ui.text(&BODY, &self.names[index]).color(INK).ellipsis().grow(1),
                ui.icon(&SMALL_ICONS, icons::CHEVRON_RIGHT).color(if selected { BLUE } else { CHEVRON }),
            ])
            .height(80)
            .padding_x(24)
            .gap(24)
            .align(Align::Center)
            .border_bottom(DIVIDER)
            .background(if selected { SELECTED } else { WHITE })
            .pressed_background(PRESSED)
            .role(Role::Button)
            .on_tap(Msg::Open(index as u16));
        match ROW_IDS.get(index) {
            Some(id) => row.id(id),
            None => row,
        }
    }
}

/// A row with its own state, reporting changes to its parent.
pub struct SortOption {
    on: bool,
}

#[derive(Clone, Copy)]
pub enum SortMsg {
    Toggle,
}

impl Component for SortOption {
    type Msg = SortMsg;
    type Props = bool;
    type Event = bool;

    fn update(&mut self, _msg: SortMsg, cx: &mut Cx<Self>) {
        self.on = !self.on;
        cx.emit(self.on);
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<SortMsg>) -> Element<'a, SortMsg> {
        ui.row([
            ui.text(&BODY, "Sort by name").color(INK).grow(1),
            ui.column([]).size(28, 28).radius(14).background(if self.on { BLUE } else { BORDER }).id("sort-mark"),
        ])
        .height(56)
        .align(Align::Center)
        .role(Role::Button)
        .id("sort-row")
        .pressed_background(PRESSED)
        .on_tap(SortMsg::Toggle)
    }
}

impl Child for SortOption {
    fn create(props: &bool) -> Self {
        Self { on: *props }
    }

    fn set_props(&mut self, props: &bool) {
        self.on = *props;
    }
}
