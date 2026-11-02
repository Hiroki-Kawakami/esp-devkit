/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::vec::Vec;

use devkit_ui::widgets::{Menu, button, checkbox, icon_button, list_item, modal, slider, slider_value, switch, top_bar};
use devkit_ui::{Align, Child, Component, Cx, Element, Handle, ListId, Overlay, RenderMode, Root, Screen, SplitRole, Strips, Ui};
use idf::heap::Caps;
use idf::sync::{LazyLock, Mutex};

use crate::resources::{colors, icons};
use super::{SPARE_STRIPS, icon};

const FILES: ListId = 0;
const FILE_COUNT: usize = 500;
const ROW_IDS: [&str; 8] = ["row0", "row1", "row2", "row3", "row4", "row5", "row6", "row7"];
const SIDEBAR_WIDTH: i16 = 400;
const VIDEO_STRIP_BYTES: usize = 2 * 64 * 1024;

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

pub struct FileManager;

impl Component for FileManager {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut Cx<Self>) {}

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        let (width, height) = ui.screen_size();
        let pane = ui.navigator::<Pane, Storages>(()).split(width > height, SIDEBAR_WIDTH).grow(1).id("pane");
        ui.column([pane]).background(ui.colors().background)
    }
}

impl Screen for FileManager {}

const SD_CARD: usize = 2;

static STORAGE_EVENTS: LazyLock<Mutex<Option<Handle<StorageMsg>>>> = LazyLock::new(|| Mutex::new(None));

/// As a card slot's detect pin would report, from any task.
pub fn set_card_present(present: bool) -> bool {
    STORAGE_EVENTS.lock().as_ref().is_some_and(|handle| handle.post(StorageMsg::Card(present)))
}

#[derive(Clone, Copy)]
pub enum StorageMsg {
    Back,
    Open(u8),
    Card(bool),
}

/// The storage list, on the left of a wide screen.
pub struct Storages {
    selected: Option<usize>,
    card: bool,
}

impl Component for Storages {
    type Msg = StorageMsg;

    fn update(&mut self, msg: StorageMsg, cx: &mut Cx<Self>) {
        match msg {
            StorageMsg::Back => cx.nav_of::<Root>().pop(),
            StorageMsg::Open(index) => {
                self.selected = Some(usize::from(index));
                cx.nav().show(Browser::new());
            }
            StorageMsg::Card(present) => {
                self.card = present;
                if !present && self.selected == Some(SD_CARD) {
                    self.selected = None;
                    cx.nav().root();
                }
            }
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<StorageMsg>) -> Element<'a, StorageMsg> {
        let open = matches!(ui.split_role(), SplitRole::Main { detail: true });
        let items: Vec<_> = STORAGES
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != SD_CARD || self.card)
            .map(|(i, name)| {
                list_item(ui, None, name, None, None, StorageMsg::Open(i as u8)).selected(open && self.selected == Some(i)).id(STORAGE_IDS[i])
            })
            .collect();
        ui.column([top_bar(ui, Some(StorageMsg::Back), "File Manager", ui.empty()), ui.column(items)])
            .padding_each(0, ui.insets().top, 0, 0)
            .background(ui.colors().background)
    }
}

impl Screen for Storages {
    fn on_enter(&mut self, cx: &mut Cx<Self>) {
        *STORAGE_EVENTS.lock() = Some(cx.handle());
    }
}

impl Child for Storages {
    fn create(_: &()) -> Self {
        Self { selected: None, card: true }
    }

    fn set_props(&mut self, _: &()) {}
}

#[derive(Clone, Copy)]
pub enum Msg {
    Open(u16),
    Back,
    OpenMenu,
    Compact,
}

pub struct Browser {
    names: Vec<String>,
    selected: Option<u16>,
    revision: u32,
}

impl Browser {
    fn new() -> Self {
        let names = (0..FILE_COUNT).map(|i| alloc::format!("{:03} {}", i, NAMES[i % NAMES.len()])).collect();
        Self { names, selected: None, revision: 0 }
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
            Msg::OpenMenu => {
                if let Some(anchor) = cx.tap_rect() {
                    cx.popup(Menu::new(anchor, &[("Compact", Msg::Compact)], cx.handle()));
                }
            }
            Msg::Compact => {}
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        let theme = ui.theme();
        let back = ui.can_pop().then_some(Msg::Back);
        let more = icon_button(ui, ui.theme().icons.more, Msg::OpenMenu).id("more");
        ui.column([
            top_bar(ui, back, "Download", more),
            ui.text(theme.fonts.caption, "Internal Storage › Download")
                .foreground(colors::MUTED)
                .height(48)
                .padding_each(24, 13, 24, 0)
                .border_top(ui.colors().divider),
            ui.list(FILES, self.names.len(), theme.widgets.list.row_height).revision(self.revision).id("files").grow(1).background(ui.colors().surface),
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(ui.colors().surface)
    }

    fn list_row<'a>(&'a self, _list: ListId, index: usize, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        let row = list_item(ui, Some(icon(FILE_ICONS[index % FILE_ICONS.len()])), &self.names[index], None, None, Msg::Open(index as u16));
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
        let (c, m) = (ui.colors(), &theme.widgets.unit);
        ui.column([
            top_bar(ui, Some(DetailMsg::Back), &self.name, ui.empty()),
            ui.column([
                ui.row([ui.text(theme.fonts.body, "Keep").foreground(c.text).grow(1), switch(ui, self.keep, DetailMsg::Keep).id("keep")]).align(Align::Center),
                ui.row([ui.text(theme.fonts.body, "Starred").foreground(c.text).grow(1), checkbox(ui, self.star, DetailMsg::Star).id("star")]).align(Align::Center),
                ui.row([ui.text(theme.fonts.body, "Video mode").foreground(c.text).grow(1), switch(ui, self.video, DetailMsg::Video).id("video")]).align(Align::Center),
                ui.text(theme.fonts.caption, ui.format(format_args!("Volume {}", self.volume))).foreground(colors::MUTED),
                slider(ui, self.volume, |d| DetailMsg::Volume(slider_value(d))).id("volume"),
                ui.row([
                    button(ui, "Rename", theme.widgets.button.secondary, DetailMsg::Back).grow(1).disabled(!self.keep).id("rename"),
                    button(ui, "Delete", theme.widgets.button.primary, DetailMsg::Ask).grow(1).id("delete"),
                ])
                .gap(m.gap),
                ui.text(theme.fonts.caption, "This note is long enough to wrap, and what follows it moves down with its height.").foreground(colors::MUTED).wrap().id("note"),
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
                ui.text(theme.fonts.title, "Delete this file?").foreground(ui.colors().text),
                ui.text(theme.fonts.body, "This cannot be undone.").foreground(colors::MUTED),
                button(ui, "Cancel", theme.widgets.button.secondary, ConfirmMsg::Close).id("cancel"),
            ])
            .gap(theme.widgets.unit.padding);
        modal(ui, content)
    }
}

impl Screen for Confirm {
    fn scrim_tap(&self) -> Option<ConfirmMsg> {
        Some(ConfirmMsg::Close)
    }
}
