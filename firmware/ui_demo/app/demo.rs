/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use devkit_gfx::{Color, TextOptions, TextRun};
use devkit_ui::widgets::{button, checkbox, icon_button, list_item, menu, menu_item, modal, slider, slider_value, switch, top_bar};
use devkit_ui::keyboard::InputKind;
use devkit_ui::widgets::{password_field, text_area, text_field};
use devkit_ui::{Align, Bars, Canvas, CanvasId, Child, Component, Cx, Element, Fonts, Gesture, Gestures, Icon, Icons, ListId, Overlay, Phase, Raster, RasterTarget, Rect, RenderMode, Root, Screen, ScreenStyle, Strips, TextBuffer, TextInput, Theme, Ui};
use idf::heap::Caps;
use idf::sync::{LazyLock, Mutex};

use crate::resources::icons;
use crate::scene::{BODY, CAPTION, ICONS, SMALL_ICONS, TITLE};

const FILES: ListId = 0;
const FILE_COUNT: usize = 500;
const ROW_IDS: [&str; 8] = ["row0", "row1", "row2", "row3", "row4", "row5", "row6", "row7"];
const SIDEBAR_WIDTH: i16 = 400;
const VIDEO_STRIP_BYTES: usize = 2 * 64 * 1024;

const FONTS: Fonts = Fonts { title: &TITLE, body: &BODY, caption: &CAPTION };
const fn icon(glyph: char) -> Icon {
    Icon { font: &ICONS, glyph }
}
const fn small_icon(glyph: char) -> Icon {
    Icon { font: &SMALL_ICONS, glyph }
}
const THEME_ICONS: Icons = Icons {
    back: icon(icons::BACK),
    more: icon(icons::MORE),
    close: icon(icons::CLOSE),
    check: icon(icons::CHECK),
    chevron: small_icon(icons::CHEVRON_RIGHT),
    shift: small_icon(icons::SHIFT),
    backspace: small_icon(icons::BACKSPACE),
    enter: small_icon(icons::ENTER),
};
pub static LIGHT: Theme = Theme::light(FONTS, THEME_ICONS);
pub static DARK: Theme = Theme::dark(FONTS, THEME_ICONS);

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
            ui.icon(small_icon(icons::WIFI)).color(color),
            ui.icon(small_icon(icons::BATTERY)).color(color),
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
    OpenGestures,
    OpenCanvas,
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
            Msg::OpenCanvas => {
                self.menu = false;
                cx.nav_of::<Root>().push(CanvasDemo::new());
            }
            Msg::OpenGestures => {
                self.menu = false;
                cx.nav_of::<Root>().push(GestureDemo::new());
            }
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        let theme = ui.theme();
        let (width, height) = ui.screen_size();
        let back = (height > width).then_some(Msg::Back);
        let more = icon_button(ui, ui.theme().icons.more, Msg::ToggleMenu).id("more");
        let popup = if self.menu {
            menu(ui, 124, [menu_item(ui, "Compact", Msg::ToggleMenu), menu_item(ui, "Settings", Msg::OpenSettings).id("open-settings"), menu_item(ui, "Gestures", Msg::OpenGestures).id("open-gestures"), menu_item(ui, "Canvas", Msg::OpenCanvas).id("open-canvas")], Msg::ToggleMenu)
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
        let (c, m) = (&theme.colors, &theme.metrics);
        ui.column([
            top_bar(ui, Some(DetailMsg::Back), &self.name, ui.empty()),
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
    Name(TextInput),
    Pin(TextInput),
    Password(TextInput),
    Note(TextInput),
}

pub struct Settings {
    dark: bool,
    name: TextBuffer,
    pin: TextBuffer,
    password: TextBuffer,
    note: TextBuffer,
}

impl Settings {
    fn new() -> Self {
        Self { dark: false, name: TextBuffer::new(), pin: TextBuffer::new(), password: TextBuffer::new(), note: TextBuffer::new() }
    }
}

fn edit(buffer: &mut TextBuffer, input: TextInput) {
    if input == TextInput::Enter {
        buffer.apply(TextInput::Char('\n'));
    } else {
        buffer.apply(input);
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
            SettingsMsg::Name(input) => {
                self.name.apply(input);
            }
            SettingsMsg::Pin(input) => {
                self.pin.apply(input);
            }
            SettingsMsg::Password(input) => {
                self.password.apply(input);
            }
            SettingsMsg::Note(input) => edit(&mut self.note, input),
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<SettingsMsg>) -> Element<'a, SettingsMsg> {
        let theme = ui.theme();
        let (c, m) = (&theme.colors, &theme.metrics);
        ui.column([
            top_bar(ui, Some(SettingsMsg::Back), "Settings", ui.empty()),
            ui.row([ui.text(theme.fonts.body, "Dark mode").color(c.on_surface).grow(1), switch(ui, self.dark, SettingsMsg::Dark).id("dark")])
                .padding_x(m.padding)
                .height(m.row_height)
                .align(Align::Center)
                .border_top(c.divider),
            ui.column([
                text_field(ui, &self.name, SettingsMsg::Name).placeholder("Device name").id("name"),
                text_field(ui, &self.pin, SettingsMsg::Pin).placeholder("PIN").input(InputKind::Number).id("pin"),
                password_field(ui, &self.password, SettingsMsg::Password).placeholder("Password").id("password"),
                text_area(ui, &self.note, SettingsMsg::Note).placeholder("Notes").id("note"),
            ])
            .padding(m.padding)
            .gap(m.gap),
        ])
        .padding_each(0, ui.insets().top, 0, ui.insets().bottom)
        .background(c.surface)
    }
}

impl Screen for Settings {
    fn style(&self) -> ScreenStyle {
        ScreenStyle { bars: if self.dark { Bars::Light } else { Bars::Dark } }
    }
}

const TERMINAL: CanvasId = 0;
const WAVE: CanvasId = 1;
const LINE_HEIGHT: i16 = 32;
const WAVE_PERIOD: i32 = 180;

struct Wave {
    phase: i32,
    height: i16,
    color: Color,
}

impl Wave {
    fn level(&self, x: i32) -> i16 {
        let t = (x + self.phase).rem_euclid(WAVE_PERIOD);
        let triangle = if t < WAVE_PERIOD / 2 { t } else { WAVE_PERIOD - t };
        (triangle * i32::from(self.height - 6) * 2 / WAVE_PERIOD) as i16
    }
}

impl Raster for Wave {
    fn raster(&self, target: &mut RasterTarget<'_>) {
        let area = target.area();
        for x in area.x..area.right() {
            target.fill(Rect::new(x, self.level(i32::from(x)), 2, 6), self.color);
        }
    }
}

#[derive(Clone, Copy)]
pub enum CanvasMsg {
    Back,
    Redraw,
    Sized(i16, i16),
    WaveSized(i16),
    Key(TextInput),
    Scrub(Gesture),
}

pub struct CanvasDemo {
    lines: Vec<String>,
    runs: Vec<TextRun>,
    columns: i16,
    rows: usize,
    wave: Wave,
    scrub_from: i32,
}

impl CanvasDemo {
    fn new() -> Self {
        Self {
            lines: alloc::vec![String::from("$ ")],
            runs: Vec::new(),
            columns: 0,
            rows: 0,
            wave: Wave { phase: 0, height: 0, color: Color::rgb(0x21, 0x96, 0xF3) },
            scrub_from: 0,
        }
    }

    fn lay_out(&mut self) {
        let shown = self.lines.len().saturating_sub(self.rows);
        self.runs.resize_with(self.lines.len() - shown, TextRun::new);
        for (run, line) in self.runs.iter_mut().zip(&self.lines[shown..]) {
            run.layout(&BODY, line, &TextOptions::default());
        }
    }

    fn line_rect(&self, row: usize) -> Rect {
        Rect::new(0, row as i16 * LINE_HEIGHT, self.columns, LINE_HEIGHT)
    }

    fn key(&mut self, input: TextInput, cx: &mut Cx<Self>) {
        let last = self.lines.len() - 1;
        match input {
            TextInput::Char(c) => self.lines[last].push(c),
            TextInput::Backspace if self.lines[last].len() > 2 => {
                self.lines[last].pop();
            }
            TextInput::Enter => {
                self.lines.push(String::from("$ "));
                self.lay_out();
                cx.redraw("term");
                return;
            }
            _ => return,
        }
        self.lay_out();
        cx.redraw_rect("term", self.line_rect(self.runs.len() - 1));
    }
}

impl Component for CanvasDemo {
    type Msg = CanvasMsg;

    fn update(&mut self, msg: CanvasMsg, cx: &mut Cx<Self>) {
        match msg {
            CanvasMsg::Back => cx.nav().pop(),
            CanvasMsg::Redraw => cx.redraw("term"),
            CanvasMsg::Sized(width, height) => {
                self.columns = width;
                self.rows = (height / LINE_HEIGHT).max(1) as usize;
                self.lay_out();
                cx.redraw("term");
            }
            CanvasMsg::WaveSized(height) => self.wave.height = height,
            CanvasMsg::Key(input) => self.key(input, cx),
            CanvasMsg::Scrub(Gesture::Pan { phase, dx, .. }) => {
                if phase == Phase::Began {
                    self.scrub_from = self.wave.phase;
                }
                self.wave.phase = self.scrub_from - i32::from(dx);
                cx.redraw("wave");
            }
            CanvasMsg::Scrub(_) => {}
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<CanvasMsg>) -> Element<'a, CanvasMsg> {
        let theme = ui.theme();
        let redraw = button(ui, "Redraw", theme.looks.button_secondary, CanvasMsg::Redraw).keep_focus().id("term-redraw");
        ui.column([
            top_bar(ui, Some(CanvasMsg::Back), "Canvas", redraw),
            ui.canvas(TERMINAL).grow(1).id("term").on_size(CanvasMsg::Sized).on_input(CanvasMsg::Key),
            ui.canvas(WAVE).height(160).id("wave").on_size(|_, height| CanvasMsg::WaveSized(height)).gestures(Gestures::PAN, CanvasMsg::Scrub),
        ])
        .padding_each(0, ui.insets().top, 0, ui.insets().bottom)
        .background(theme.colors.surface)
    }

    fn draw<'a>(&'a self, id: CanvasId, canvas: &mut Canvas<'a, '_>) {
        let (width, height) = canvas.size();
        let colors = canvas.theme().colors;
        if id == WAVE {
            canvas.fill(Rect::new(0, 0, width, height), colors.background);
            canvas.raster(Rect::new(0, 0, width, height), &self.wave);
            return;
        }
        canvas.fill(Rect::new(0, 0, width, height), Color::rgb(0x10, 0x14, 0x18));
        for (row, run) in self.runs.iter().enumerate() {
            let rect = Rect::new(0, row as i16 * LINE_HEIGHT, width, LINE_HEIGHT);
            if canvas.needs(rect) {
                canvas.text(8, rect.y, run, Color::rgb(0xE0, 0xE6, 0xEA));
            }
        }
    }
}

impl Screen for CanvasDemo {}

const BOX_SIZE: i16 = 160;

#[derive(Clone, Copy)]
pub enum GestureMsg {
    Back,
    Tap,
    Touch(Gesture),
}

pub struct GestureDemo {
    label: String,
    position: (i16, i16),
    from: (i16, i16),
    scale: f32,
    base: f32,
}

impl GestureDemo {
    fn new() -> Self {
        Self { label: String::from("Touch the area"), position: (240, 300), from: (240, 300), scale: 1.0, base: 1.0 }
    }
}

impl Component for GestureDemo {
    type Msg = GestureMsg;

    fn update(&mut self, msg: GestureMsg, cx: &mut Cx<Self>) {
        self.label.clear();
        let label = &mut self.label;
        let _ = match msg {
            GestureMsg::Back => {
                cx.nav().pop();
                return;
            }
            GestureMsg::Tap => write!(label, "tap"),
            GestureMsg::Touch(Gesture::Down { x, y }) => write!(label, "down {x},{y}"),
            GestureMsg::Touch(Gesture::Up { x, y }) => write!(label, "up {x},{y}"),
            GestureMsg::Touch(Gesture::Pan { phase, dx, dy, .. }) => {
                if phase == Phase::Began {
                    self.from = self.position;
                }
                self.position = (self.from.0 + dx, self.from.1 + dy);
                write!(label, "pan {phase:?} {dx},{dy}")
            }
            GestureMsg::Touch(Gesture::Pinch { phase, scale, .. }) => {
                if phase == Phase::Began {
                    self.base = self.scale;
                }
                self.scale = (self.base * scale).clamp(0.25, 4.0);
                write!(label, "pinch {phase:?} {scale:.2}")
            }
            GestureMsg::Touch(Gesture::Rotate { phase, angle, .. }) => write!(label, "rotate {phase:?} {angle:.2}"),
            GestureMsg::Touch(Gesture::DoubleTap { .. }) => {
                self.scale = 1.0;
                write!(label, "double tap")
            }
            GestureMsg::Touch(Gesture::LongPress { x, y }) => write!(label, "long press {x},{y}"),
            GestureMsg::Touch(Gesture::Swipe { direction }) => write!(label, "swipe {direction:?}"),
        };
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<GestureMsg>) -> Element<'a, GestureMsg> {
        let theme = ui.theme();
        let c = &theme.colors;
        let side = (f32::from(BOX_SIZE) * self.scale) as i16;
        let (x, y) = (self.position.0 - side / 2, self.position.1 - side / 2);
        ui.column([
            top_bar(ui, Some(GestureMsg::Back), "Gestures", ui.empty()),
            ui.text(theme.fonts.body, &self.label).padding(theme.metrics.padding).id("gesture-label"),
            ui.column([ui.column([]).size(side, side).radius(theme.metrics.radius).background(c.primary).absolute(Some(x), Some(y), None, None).id("gesture-box")])
                .grow(1)
                .clip()
                .id("gesture-area")
                .gestures(Gestures::all(), GestureMsg::Touch)
                .on_tap(GestureMsg::Tap),
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(c.surface)
    }
}

impl Screen for GestureDemo {}

pub const INSET_TOP: i16 = 52;
