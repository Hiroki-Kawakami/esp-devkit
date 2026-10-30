/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::vec::Vec;

use devkit_ui::widgets::{button, checkbox, icon_button, list_item, menu, menu_item, modal, slider, slider_value, switch, top_bar};
use devkit_ui::{Align, Bars, Child, Component, Cx, Element, Fonts, ListId, Overlay, RenderMode, Root, Screen, ScreenStyle, Strips, Theme, Ui};
use idf::heap::Caps;
use idf::sync::{LazyLock, Mutex};

use crate::resources::icons;
use crate::scene::{BODY, CAPTION, ICONS, SMALL_ICONS, TITLE};

const FILES: ListId = 0;
const FILE_COUNT: usize = 500;
const ROW_IDS: [&str; 8] = ["row0", "row1", "row2", "row3", "row4", "row5", "row6", "row7"];
const SIDEBAR_WIDTH: i16 = 400;
const VIDEO_STRIP_BYTES: usize = 2 * 64 * 1024;

const FONTS: Fonts = Fonts { title: &TITLE, body: &BODY, caption: &CAPTION, icons: &ICONS };
pub static LIGHT: Theme = Theme::light(FONTS);
pub static DARK: Theme = Theme::dark(FONTS);

/// The strip memory not in use.
pub static SPARE_STRIPS: LazyLock<Mutex<Option<Strips>>> = LazyLock::new(|| Mutex::new(None));

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
const STORAGES: [&str; 3] = ["Internal Storage", "System", "SD Card"];
const STORAGE_IDS: [&str; 3] = ["storage0", "storage1", "storage2"];

/// Marks the navigator of the file pages.
pub struct Pane;

pub struct StatusBar {
    seconds: u32,
}

#[derive(Clone, Copy)]
pub enum StatusMsg {
    Tick,
}

impl StatusBar {
    pub fn new() -> Self {
        Self { seconds: 5 * 3600 + 29 * 60 }
    }
}

impl Component for StatusBar {
    type Msg = StatusMsg;

    fn update(&mut self, _msg: StatusMsg, cx: &mut Cx<Self>) {
        self.seconds += 1;
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<StatusMsg>) -> Element<'a, StatusMsg> {
        if ui.screen_style().bars == Bars::Hidden {
            return ui.empty();
        }
        let color = ui.theme().colors.on_surface;
        let time = ui.format(format_args!("{:02}:{:02}:{:02}", self.seconds / 3600, self.seconds / 60 % 60, self.seconds % 60));
        ui.row([
            ui.text(&CAPTION, time).color(color).id("clock").grow(1),
            ui.icon(&SMALL_ICONS, icons::WIFI).color(color),
            ui.icon(&SMALL_ICONS, icons::BATTERY).color(color),
            ui.text(&CAPTION, "78%").color(color),
        ])
        .height(ui.insets().top)
        .padding_x(28)
        .gap(14)
        .align(Align::Center)
    }
}

/// The root screen: pages fill the screen when upright, and sit next to the
/// storage list when wide.
pub struct FileManager;

#[derive(Clone, Copy)]
pub enum ManagerMsg {
    Open,
}

impl FileManager {
    fn storages<'a>(&'a self, ui: &'a Ui<ManagerMsg>) -> Element<'a, ManagerMsg> {
        let items: Vec<_> = STORAGES
            .iter()
            .enumerate()
            .map(|(i, name)| list_item(ui, None, name, None, None, ManagerMsg::Open).id(STORAGE_IDS[i]))
            .collect();
        ui.column(items)
    }
}

impl Component for FileManager {
    type Msg = ManagerMsg;

    fn update(&mut self, _msg: ManagerMsg, cx: &mut Cx<Self>) {
        cx.nav_of::<Pane>().root();
    }

    fn view<'a>(&'a self, ui: &'a Ui<ManagerMsg>) -> Element<'a, ManagerMsg> {
        let (width, height) = ui.screen_size();
        let c = &ui.theme().colors;
        if width > height {
            ui.row([
                ui.column([top_bar(ui, None, "File Manager", ui.empty()), self.storages(ui)])
                    .width(SIDEBAR_WIDTH)
                    .padding_each(0, ui.insets().top, 0, 0)
                    .background(c.background),
                ui.navigator::<Pane, Browser>(()).grow(1).id("pane"),
            ])
        } else {
            ui.column([ui.navigator::<Pane, Storages>(()).grow(1).id("pane")]).background(c.background)
        }
    }
}

impl Screen for FileManager {}

/// The storage list as a page of its own, on an upright screen.
pub struct Storages;

impl Component for Storages {
    type Msg = ManagerMsg;

    fn update(&mut self, _msg: ManagerMsg, cx: &mut Cx<Self>) {
        cx.nav().push(Browser::new());
    }

    fn view<'a>(&'a self, ui: &'a Ui<ManagerMsg>) -> Element<'a, ManagerMsg> {
        let items: Vec<_> = STORAGES
            .iter()
            .enumerate()
            .map(|(i, name)| list_item(ui, None, name, None, None, ManagerMsg::Open).id(STORAGE_IDS[i]))
            .collect();
        ui.column([top_bar(ui, None, "File Manager", ui.empty()), ui.column(items)])
            .padding_each(0, ui.insets().top, 0, 0)
            .background(ui.theme().colors.background)
    }
}

impl Screen for Storages {}

impl Child for Storages {
    fn create(_: &()) -> Self {
        Self
    }

    fn set_props(&mut self, _: &()) {}
}

#[derive(Clone, Copy)]
pub enum Msg {
    Open(u16),
    Back,
    ToggleMenu,
    OpenSettings,
}

pub struct Browser {
    names: Vec<String>,
    selected: Option<u16>,
    menu: bool,
    revision: u32,
}

impl Browser {
    fn new() -> Self {
        let names = (0..FILE_COUNT).map(|i| alloc::format!("{:03} {}", i, NAMES[i % NAMES.len()])).collect();
        Self { names, selected: None, menu: false, revision: 0 }
    }
}

impl Component for Browser {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, cx: &mut Cx<Self>) {
        match msg {
            Msg::Open(i) => {
                self.selected = Some(i);
                self.revision += 1;
                cx.nav().push(Detail::new(&self.names[usize::from(i)]));
            }
            Msg::Back => cx.nav().pop(),
            Msg::ToggleMenu => self.menu = !self.menu,
            Msg::OpenSettings => {
                self.menu = false;
                cx.nav_of::<Root>().push(Settings::new());
            }
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        let theme = ui.theme();
        let (width, height) = ui.screen_size();
        let back = (height > width).then_some((icons::BACK, Msg::Back));
        let more = icon_button(ui, icons::MORE, Msg::ToggleMenu).id("more");
        let popup = if self.menu {
            menu(ui, 124, [menu_item(ui, "Compact", Msg::ToggleMenu), menu_item(ui, "Settings", Msg::OpenSettings).id("open-settings")], Msg::ToggleMenu)
        } else {
            ui.empty()
        };
        ui.column([
            top_bar(ui, back, "Download", more),
            ui.text(theme.fonts.caption, "Internal Storage › Download")
                .color(theme.colors.muted)
                .height(48)
                .padding_each(24, 13, 24, 0)
                .border_top(theme.colors.divider),
            ui.list(FILES, self.names.len(), theme.metrics.row_height).revision(self.revision).id("files").grow(1).background(theme.colors.surface),
            popup,
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(theme.colors.surface)
    }

    fn list_row<'a>(&'a self, _list: ListId, index: usize, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        let row = list_item(ui, Some(FILE_ICONS[index % FILE_ICONS.len()]), &self.names[index], None, None, Msg::Open(index as u16));
        let row = row.selected(self.selected == Some(index as u16));
        match ROW_IDS.get(index) {
            Some(id) => row.id(id),
            None => row,
        }
    }
}

impl Screen for Browser {}

impl Child for Browser {
    fn create(_: &()) -> Self {
        Self::new()
    }

    fn set_props(&mut self, _: &()) {}
}

#[derive(Clone, Copy)]
pub enum DetailMsg {
    Back,
    Volume(u8),
    Keep,
    Star,
    Ask,
    Video,
    VideoReady,
}

pub struct Detail {
    name: String,
    volume: u8,
    keep: bool,
    star: bool,
    video: bool,
}

impl Detail {
    fn new(name: &str) -> Self {
        Self { name: String::from(name), volume: 128, keep: false, star: true, video: false }
    }

    fn toggle_video(&mut self, cx: &mut Cx<Self>) {
        let parked = SPARE_STRIPS.lock().take();
        let strips = match parked {
            Some(strips) => Some(strips),
            None => Strips::allocate(VIDEO_STRIP_BYTES, Caps::SPIRAM).ok(),
        };
        if let Some(strips) = strips {
            let lanes = if self.video { 2 } else { 1 };
            cx.set_render_mode(RenderMode { strips, lanes }, DetailMsg::VideoReady);
            self.video = !self.video;
        }
    }
}

impl Component for Detail {
    type Msg = DetailMsg;

    fn update(&mut self, msg: DetailMsg, cx: &mut Cx<Self>) {
        match msg {
            DetailMsg::Back => cx.nav().pop(),
            DetailMsg::Volume(value) => self.volume = value,
            DetailMsg::Keep => self.keep = !self.keep,
            DetailMsg::Star => self.star = !self.star,
            DetailMsg::Ask => cx.nav_of::<Overlay>().push(Confirm),
            DetailMsg::Video => self.toggle_video(cx),
            DetailMsg::VideoReady => *SPARE_STRIPS.lock() = cx.take_strips(),
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<DetailMsg>) -> Element<'a, DetailMsg> {
        let theme = ui.theme();
        let (c, m) = (&theme.colors, &theme.metrics);
        ui.column([
            top_bar(ui, Some((icons::BACK, DetailMsg::Back)), &self.name, ui.empty()),
            ui.column([
                ui.row([ui.text(theme.fonts.body, "Keep").color(c.on_surface).grow(1), switch(ui, self.keep, DetailMsg::Keep).id("keep")]).align(Align::Center),
                ui.row([ui.text(theme.fonts.body, "Starred").color(c.on_surface).grow(1), checkbox(ui, self.star, DetailMsg::Star).id("star")]).align(Align::Center),
                ui.row([ui.text(theme.fonts.body, "Video mode").color(c.on_surface).grow(1), switch(ui, self.video, DetailMsg::Video).id("video")]).align(Align::Center),
                ui.text(theme.fonts.caption, ui.format(format_args!("Volume {}", self.volume))).color(c.muted),
                slider(ui, self.volume, |d| DetailMsg::Volume(slider_value(d))).id("volume"),
                ui.row([
                    button(ui, "Rename", theme.looks.button_secondary, DetailMsg::Back).grow(1).disabled(!self.keep).id("rename"),
                    button(ui, "Delete", theme.looks.button, DetailMsg::Ask).grow(1).id("delete"),
                ])
                .gap(m.gap),
                ui.text(theme.fonts.caption, "This note is long enough to wrap, and what follows it moves down with its height.").color(c.muted).wrap().id("note"),
            ])
            .padding(m.padding)
            .gap(m.padding)
            .border_top(c.divider),
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(c.surface)
    }
}

impl Screen for Detail {}

pub struct Confirm;

#[derive(Clone, Copy)]
pub enum ConfirmMsg {
    Close,
}

impl Component for Confirm {
    type Msg = ConfirmMsg;

    fn update(&mut self, _msg: ConfirmMsg, cx: &mut Cx<Self>) {
        cx.nav_of::<Overlay>().pop();
    }

    fn view<'a>(&'a self, ui: &'a Ui<ConfirmMsg>) -> Element<'a, ConfirmMsg> {
        let theme = ui.theme();
        let content = ui
            .column([
                ui.text(theme.fonts.title, "Delete this file?").color(theme.colors.on_surface),
                ui.text(theme.fonts.body, "This cannot be undone.").color(theme.colors.muted),
                button(ui, "Cancel", theme.looks.button_secondary, ConfirmMsg::Close).id("cancel"),
            ])
            .gap(theme.metrics.padding);
        modal(ui, content, ConfirmMsg::Close)
    }
}

impl Screen for Confirm {}

#[derive(Clone, Copy)]
pub enum SettingsMsg {
    Back,
    Dark,
}

pub struct Settings {
    dark: bool,
}

impl Settings {
    fn new() -> Self {
        Self { dark: false }
    }
}

impl Component for Settings {
    type Msg = SettingsMsg;

    fn update(&mut self, msg: SettingsMsg, cx: &mut Cx<Self>) {
        match msg {
            SettingsMsg::Back => cx.nav().pop(),
            SettingsMsg::Dark => {
                self.dark = !self.dark;
                cx.set_theme(if self.dark { &DARK } else { &LIGHT });
            }
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<SettingsMsg>) -> Element<'a, SettingsMsg> {
        let theme = ui.theme();
        let (c, m) = (&theme.colors, &theme.metrics);
        ui.column([
            top_bar(ui, Some((icons::BACK, SettingsMsg::Back)), "Settings", ui.empty()),
            ui.row([ui.text(theme.fonts.body, "Dark mode").color(c.on_surface).grow(1), switch(ui, self.dark, SettingsMsg::Dark).id("dark")])
                .padding_x(m.padding)
                .height(m.row_height)
                .align(Align::Center)
                .border_top(c.divider),
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(c.surface)
    }
}

impl Screen for Settings {
    fn style(&self) -> ScreenStyle {
        ScreenStyle { bars: if self.dark { Bars::Light } else { Bars::Dark } }
    }
}

pub const INSET_TOP: i16 = 52;
