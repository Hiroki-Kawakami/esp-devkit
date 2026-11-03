/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::Relaxed;

use devkit_gfx::format::{self, FontHeader, GlyphRecord};
use devkit_gfx::{Color, Font, FontChain, Rect};
use idf::heap::Caps;

use crate::*;
use crate::erased::Erased;
use crate::external::ExternalSurface;
use crate::nav::SplitRole;
use crate::rotation::Rotations;
use crate::gesture::{Gesture, Gestures};
use crate::keyboard::{InputKind, Key, KeyAction, KeyLabel, Keyboard, Layer};
use crate::node::{NONE, Node, NodeContent, NodeId};
use crate::tree::Tree;
use crate::widgets;

const SCREEN: Rect = Rect::new(0, 0, 400, 600);
const WHITE: Color = Color::rgb(255, 255, 255);
const RED: Color = Color::rgb(255, 0, 0);
const BLUE: Color = Color::rgb(0, 0, 255);

fn chain() -> &'static FontChain {
    let mut records = Vec::new();
    let mut codes = Vec::new();
    let mut data = Vec::new();
    for c in 32u32..127 {
        codes.extend_from_slice(&c.to_le_bytes());
        let bitmap = data.len() as u32;
        data.extend(format::pack_levels(&[15; 6 * 8], 4));
        GlyphRecord { bitmap, advance: 8 * 16, width: 6, height: 8, ofs_x: 0, ofs_y: 0 }.write(&mut records);
    }
    data.extend_from_slice(&[0; format::FONT_DATA_SLACK]);
    let count = codes.len() as u32 / 4;
    let table = format::FONT_HEADER_SIZE as u32 + count * 4;
    let mut out = Vec::new();
    FontHeader {
        bpp: 4,
        flags: 0,
        px: 10,
        line_height: 12,
        base_line: 2,
        max_ascent: 10,
        max_descent: 2,
        underline_position: 0,
        underline_thickness: 1,
        glyph_count: count,
        codepoints: format::FONT_HEADER_SIZE as u32,
        glyphs: table,
        data: table + records.len() as u32,
        data_len: data.len() as u32,
    }
    .write(&mut out);
    out.extend_from_slice(&codes);
    out.extend_from_slice(&records);
    out.extend_from_slice(&data);
    let font: &'static Font = Box::leak(Box::new(Font::parse(Vec::leak(out))));
    Box::leak(Box::new(FontChain::new(Vec::leak(alloc::vec![font]))))
}

static EMPTY: StateStyle = StateStyle::EMPTY;
static SLIDER_ROOT: StateStyle = StateStyle { base: &[Prop::Height(48), Prop::PaddingX(14)], ..StateStyle::EMPTY };
static SLIDER_TRACK: StateStyle = StateStyle { base: &[Prop::Height(8), Prop::Background(ColorRole::Custom(0))], ..StateStyle::EMPTY };
static SLIDER_FILL: StateStyle = StateStyle { base: &[Prop::Background(ColorRole::Primary)], ..StateStyle::EMPTY };
static SLIDER_THUMB: StateStyle = StateStyle { base: &[Prop::Width(28), Prop::Height(28)], ..StateStyle::EMPTY };
static ICON_BUTTON: StateStyle = StateStyle { base: &[Prop::Width(64), Prop::Height(64)], ..StateStyle::EMPTY };
static TOP_BAR: StateStyle = StateStyle { base: &[Prop::Height(80), Prop::PaddingX(8), Prop::Gap(12)], ..StateStyle::EMPTY };
static TOP_BAR_BACK: StateStyle = StateStyle { base: &[Prop::Height(64), Prop::Gap(12)], ..StateStyle::EMPTY };
static DROPDOWN_ITEM: StateStyle = StateStyle { base: &[Prop::Height(56)], ..StateStyle::EMPTY };
static DROPDOWN_MENU: StateStyle = StateStyle { base: &[Prop::Radius(14), Prop::Border(ColorRole::Divider, 2)], ..StateStyle::EMPTY };
static MENU_PANEL: StateStyle = StateStyle { base: &[Prop::Width(200), Prop::PaddingY(8)], ..StateStyle::EMPTY };
static MENU_ITEM: StateStyle = StateStyle { base: &[Prop::Height(50)], ..StateStyle::EMPTY };
static TABLE_ROW: StateStyle = StateStyle { base: &[Prop::MinHeight(88), Prop::PaddingX(24), Prop::Gap(12)], ..StateStyle::EMPTY };
static DIVIDER_VERTICAL: StateStyle = StateStyle { base: &[Prop::PaddingY(12)], ..StateStyle::EMPTY };
static KEY: StateStyle = StateStyle { states: &[(State::PRESSED, &[Prop::Background(ColorRole::Custom(1))])], ..StateStyle::EMPTY };

const KEY_PITCH: i16 = 88;
const TEXT_KEYS_HEIGHT: i16 = 4 * KEY_PITCH + 8;
const BACKSPACE: Key = Key::action(KeyLabel::None, KeyAction::Press(TextInput::Backspace)).width(1.5).repeat();
const SPREAD: Key = Key::spacer(7.0);
const TO_LOWER: Key = Key::action(KeyLabel::None, KeyAction::layer(0)).width(1.5);
const SYMBOLS: Key = Key::action(KeyLabel::Text("?123"), KeyAction::layer(3)).width(1.5);

static TEXT_KEYS: Keyboard = Keyboard {
    height: TEXT_KEYS_HEIGHT,
    max_height: 1.0,
    layers: &[
        Layer {
            columns: 10,
            rows: &[
                &[Key::text("q"), Key::text("w")],
                &[Key::text("a")],
                &[Key::action(KeyLabel::None, KeyAction::layer(1).once().double(2)).width(1.5), SPREAD, BACKSPACE],
                &[SYMBOLS],
            ],
        },
        Layer {
            columns: 10,
            rows: &[
                &[Key::text("Q"), Key::text("W")],
                &[Key::text("A")],
                &[Key::action(KeyLabel::None, KeyAction::layer(0).double(2)).width(1.5), SPREAD, BACKSPACE],
                &[SYMBOLS],
            ],
        },
        Layer {
            columns: 10,
            rows: &[&[Key::text("Q"), Key::text("W")], &[Key::text("A")], &[TO_LOWER, SPREAD, BACKSPACE], &[SYMBOLS]],
        },
        Layer {
            columns: 4,
            rows: &[&[Key::text("1"), Key::text("2")], &[Key::action(KeyLabel::Text("ABC"), KeyAction::layer(0)), Key::text("3")]],
        },
    ],
};

static NUMBER_KEYS: Keyboard = Keyboard {
    height: 200,
    max_height: 0.4,
    layers: &[Layer { columns: 3, rows: &[&[Key::text("1"), Key::text("2"), Key::text("3")]] }],
};

static WIDGETS: Widgets = Widgets {
    unit: UnitTheme { padding: 24, gap: 12, radius: 14 },
    button: ButtonTheme { root: &EMPTY, label: &EMPTY, primary: &EMPTY, secondary: &EMPTY, destructive: &EMPTY, plain: &EMPTY, navigation: &EMPTY, primary_outline: &EMPTY, secondary_outline: &EMPTY, destructive_outline: &EMPTY, plain_outline: &EMPTY },
    icon_button: IconButtonTheme { root: &ICON_BUTTON },
    list: ListTheme { row_height: 80 },
    list_item: ListItemTheme { root: &EMPTY, text: &EMPTY, title: &EMPTY, meta: &EMPTY, leading: &EMPTY, trailing: &EMPTY },
    top_bar: TopBarTheme { back_height: 64, padding_x: 8, root: &TOP_BAR, title: &EMPTY, back: &TOP_BAR_BACK },
    switch: SwitchTheme { root: &EMPTY, thumb: &EMPTY },
    checkbox: CheckboxTheme { root: &EMPTY, mark: &EMPTY },
    slider: SliderTheme { inset: 14, track_inset: 4, root: &SLIDER_ROOT, track: &SLIDER_TRACK, fill: &SLIDER_FILL, thumb: &SLIDER_THUMB },
    progress_bar: ProgressBarTheme { segment: 30, period_ms: 1000, track: &EMPTY, fill: &EMPTY },
    progress_indicator: ProgressIndicatorTheme { stroke: 4, sweep: 270, period_ms: 1000, track_color: ColorRole::Divider, fill_color: ColorRole::Primary, root: &EMPTY },
    segmented: SegmentedTheme { root: &EMPTY, item: &EMPTY, label: &EMPTY },
    dropdown: DropdownTheme { height: 56, root: &DROPDOWN_ITEM, label: &EMPTY, icon: &EMPTY },
    dropdown_menu: DropdownMenuTheme { root: &DROPDOWN_MENU, item: &DROPDOWN_ITEM },
    divider: DividerTheme { thickness: 1, horizontal: &EMPTY, vertical: &DIVIDER_VERTICAL, line: &EMPTY },
    table: TableTheme { padding: 24 },
    table_header: TableHeaderTheme { root: &EMPTY },
    table_section: TableSectionTheme { root: &EMPTY },
    table_row: TableRowTheme { root: &TABLE_ROW, label: &EMPTY, value: &EMPTY, chevron: &EMPTY },
    menu: MenuTheme { width: 200, padding_y: 8, panel: &MENU_PANEL },
    menu_item: MenuItemTheme { height: 50, root: &MENU_ITEM, label: &EMPTY },
    modal: ModalTheme { width: 360, margin: 24, panel: &EMPTY, title: &EMPTY, message: &EMPTY, actions: &EMPTY },
    overlay: OverlayTheme { scrim: ColorRole::Fixed(Color::rgba(0, 0, 0, 128)) },
    toast: ToastTheme { root: &EMPTY, label: &EMPTY },
    text_field: TextFieldTheme { placeholder_color: ColorRole::Custom(2), caret_color: ColorRole::Primary, underline_color: ColorRole::Primary, area_min_height: 160, root: &EMPTY },
    keyboard: KeyboardTheme { gap: 8, root: &EMPTY, label: &EMPTY, key: &KEY, special: &KEY, primary: &KEY },
};

static CUSTOM_COLORS: [Color; 3] = [Color::rgb(227, 242, 253), Color::rgb(223, 227, 230), Color::rgb(120, 144, 156)];

fn theme() -> &'static Theme {
    let chain = chain();
    let icon = Icon { font: chain, glyph: 'A' };
    let icons = Icons { back: icon, more: icon, close: icon, check: icon, chevron: icon, expand: icon, shift: icon, backspace: icon, enter: icon };
    let colors = Palette {
        text: Color::rgb(29, 33, 38),
        primary: Color::rgb(33, 150, 243),
        surface: Color::rgb(255, 255, 255),
        background: Color::rgb(233, 234, 236),
        divider: Color::rgb(217, 221, 224),
        custom: &CUSTOM_COLORS,
    };
    Box::leak(Box::new(Theme {
        light: colors,
        dark: None,
        fonts: Fonts { title: chain, body: chain, caption: chain },
        icons,
        widgets: WIDGETS,
        keyboards: Keyboards { text: &TEXT_KEYS, number: &NUMBER_KEYS },
        motion: Motion::DEFAULT,
    }))
}

fn run(tree: &mut Tree) -> Vec<Rect> {
    tree.update();
    tree.take_damage()
}

fn rect_of(tree: &Tree, id: &str) -> Rect {
    let found = tree.find(id).unwrap_or_else(|| panic!("no {id} in {}", tree.describe()));
    tree.nodes[found as usize].screen
}

fn tap(tree: &mut Tree, id: &str) {
    let point = tree.tap_point(id).unwrap();
    tap_at(tree, point);
}

fn tap_at(tree: &mut Tree, point: (i16, i16)) {
    tree.touch(&[(0, point)], 0);
    tree.touch(&[], 10);
    tree.update();
}

static PRESS_STYLE: StateStyle = StateStyle {
    font: None,
    base: &[Prop::Background(ColorRole::Fixed(RED))],
    states: &[(State::PRESSED, &[Prop::Background(ColorRole::Fixed(WHITE))]), (State::DISABLED, &[Prop::Opacity(100)])],
};

#[derive(Clone, Copy)]
enum Msg {
    Inc,
    Toggle,
    Reorder,
    FromChild(u8),
}

struct Sample {
    font: &'static FontChain,
    count: u32,
    panel: bool,
    swapped: bool,
    label: alloc::string::String,
    views: &'static core::cell::Cell<u32>,
}

impl Sample {
    fn new() -> Self {
        Self {
            font: chain(),
            count: 0,
            panel: false,
            swapped: false,
            label: alloc::string::String::from("start"),
            views: Box::leak(Box::new(core::cell::Cell::new(0))),
        }
    }
}

impl Component for Sample {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, cx: &mut Cx<Self>) {
        match msg {
            Msg::Inc => self.count += 1,
            Msg::Toggle => self.panel = !self.panel,
            Msg::Reorder => self.swapped = !self.swapped,
            Msg::FromChild(v) => self.count += u32::from(v),
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        self.views.set(self.views.get() + 1);
        let a = ui.text(self.font, "A").key(1).id("a");
        let b = ui.text(self.font, "BB").key(2).id("b");
        let (first, second) = if self.swapped { (b, a) } else { (a, b) };
        ui.column([
            ui.row([
                ui.text(self.font, ui.format(format_args!("n={}", self.count))).id("count").grow(1),
                ui.column([]).size(40, 30).style(&PRESS_STYLE).id("button").on_tap(Msg::Inc),
            ])
            .height(40)
            .padding_x(10)
            .align(Align::Center),
            ui.row([first, second]).gap(4).id("pair"),
            ui.child_mapped::<Counter>(self.count > 100, Msg::FromChild).id("child"),
            ui.list(3, 1000, 50).id("list").grow(1),
            if self.panel {
                ui.column([ui.text(self.font, &self.label)]).size(100, 100).absolute(None, Some(20), Some(10), None).background(WHITE).id("panel").on_tap(Msg::Toggle)
            } else {
                ui.empty()
            },
        ])
    }

    fn list_row<'a>(&'a self, list: ListId, index: usize, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        assert_eq!(list, 3);
        let name = if index == 2 { "row2" } else { "row" };
        ui.row([ui.text(self.font, ui.format(format_args!("{index}")))]).id(name).on_tap(Msg::Toggle).background(WHITE)
    }
}

struct Counter {
    value: u8,
    big: bool,
    views: u32,
}

#[derive(Clone, Copy)]
enum CounterMsg {
    Bump,
}

impl Component for Counter {
    type Msg = CounterMsg;
    type Props = bool;
    type Event = u8;

    fn update(&mut self, _msg: CounterMsg, cx: &mut Cx<Self>) {
        self.value += 1;
        cx.emit(self.value * 10);
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<CounterMsg>) -> Element<'a, CounterMsg> {
        let height = if self.big { 60 } else { 20 + self.value as i16 };
        ui.column([]).height(height).id("counter").on_tap(CounterMsg::Bump)
    }
}

impl Screen for Sample {}

impl Child for Counter {
    fn create(props: &bool) -> Self {
        Self { value: 0, big: *props, views: 0 }
    }

    fn set_props(&mut self, props: &bool) {
        self.big = *props;
        self.views += 1;
    }
}

#[test]
fn flex_lays_out_fixed_grow_and_cross_alignment() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    assert_eq!(rect_of(&tree, "button"), Rect::new(350, 5, 40, 30));
    let count = rect_of(&tree, "count");
    assert_eq!((count.x, count.w), (10, 340));
    assert_eq!(rect_of(&tree, "pair").y, 40);
    assert_eq!(rect_of(&tree, "pair").w, 400);
    assert_eq!(rect_of(&tree, "a").w, 8);
    assert_eq!(rect_of(&tree, "b").x, 12);
    let list = rect_of(&tree, "list");
    assert_eq!(list.y + list.h, 600);
}

struct Bar {
    back: bool,
    title: &'static str,
    taps: &'static AtomicU32,
}

impl Screen for Bar {}

impl Component for Bar {
    type Msg = ();

    fn update(&mut self, _msg: (), _cx: &mut Cx<Self>) {
        self.taps.fetch_add(1, Relaxed);
    }

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        let more = widgets::icon_button(ui, ui.theme().icons.more, ()).id("more");
        ui.column([widgets::top_bar(ui, self.back.then_some(()), self.title, more)])
    }
}

fn bar_tree(back: bool, title: &'static str, taps: &'static AtomicU32) -> Tree {
    let mut tree = Tree::new(Bar { back, title, taps }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    tree
}

fn text_node<'a>(tree: &'a Tree, wanted: &str) -> &'a Node {
    let found = tree.nodes.iter().find(|node| matches!(&node.content, NodeContent::Text(text) if text.text() == wanted));
    found.unwrap_or_else(|| panic!("no {wanted} in {}", tree.describe()))
}

fn text_rect(tree: &Tree, wanted: &str) -> Rect {
    text_node(tree, wanted).screen
}

#[test]
fn top_bar_insets_a_lone_title_to_the_padding_and_sizes_the_back_button() {
    let taps = Box::leak(Box::new(AtomicU32::new(0)));
    let alone = bar_tree(false, "Title", taps);
    let title = text_node(&alone, "Title");
    assert_eq!(title.screen.x + title.style.padding[0], 24);
    assert_eq!(title.style.padding[2], 16);
    let tree = bar_tree(true, "Title", taps);
    assert_eq!(rect_of(&tree, "back").x, 8);
    // 64 px button around a 10 px icon and a 10 px title.
    assert_eq!(text_rect(&tree, "A").x, 8 + 27);
    assert_eq!(rect_of(&tree, "back").right(), text_rect(&tree, "Title").right() + 27);
}

#[test]
fn top_bar_title_goes_back_and_the_rest_of_the_bar_does_not() {
    let taps = Box::leak(Box::new(AtomicU32::new(0)));
    let mut tree = bar_tree(true, "Title", taps);
    let (title, back) = (text_rect(&tree, "Title"), rect_of(&tree, "back"));
    let y = title.y + title.h / 2;
    for (x, count) in [(title.x + title.w / 2, 1), (back.right() + 20, 1)] {
        tree.touch(&[(0, (x, y))], 0);
        tree.touch(&[], 10);
        tree.update();
        assert_eq!(taps.load(Relaxed), count);
    }
}

#[test]
fn top_bar_long_title_stops_at_the_actions() {
    let taps = Box::leak(Box::new(AtomicU32::new(0)));
    let tree = bar_tree(true, "A very long title that cannot fit in the bar next to the actions at all", taps);
    assert!(rect_of(&tree, "back").right() + 12 <= rect_of(&tree, "more").x);
    assert_eq!(rect_of(&tree, "more").w, 64);
    assert_eq!(text_rect(&tree, "A").w, 8);
}

struct Choices {
    choice: usize,
}

impl Screen for Choices {}

impl Component for Choices {
    type Msg = usize;

    fn update(&mut self, msg: usize, cx: &mut Cx<Self>) {
        self.choice = msg;
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<usize>) -> Element<'a, usize> {
        let options = widgets::segmented(ui, &[("Off", 0), ("On", 1)], self.choice).id("choice");
        let slider = ui.column([widgets::slider(ui, 0, |_| 0).id("slider")]).grow(1);
        ui.column([widgets::table_section(
            ui,
            [
                widgets::table_row(ui, [widgets::table_label(ui, "Mode"), options]),
                widgets::table_row(ui, [widgets::table_label(ui, "Next"), widgets::table_value(ui, "Value"), widgets::table_chevron(ui)]).on_tap(1).id("next"),
                widgets::table_row(ui, [slider]).id("tall").min_height(100),
            ],
        )
        .id("section")])
    }
}

#[test]
fn a_segmented_control_marks_the_tapped_option() {
    let mut tree = Tree::new(Choices { choice: 0 }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let selected = |tree: &Tree, label: &str| tree.nodes[text_node(tree, label).parent as usize].state.contains(State::SELECTED);
    assert!(selected(&tree, "Off") && !selected(&tree, "On"));
    let on = text_rect(&tree, "On");
    tree.touch(&[(0, (on.x + 1, on.y + 1))], 0);
    tree.touch(&[], 10);
    tree.update();
    assert!(!selected(&tree, "Off") && selected(&tree, "On"));
}

#[test]
fn a_table_section_divides_its_rows_and_they_keep_its_width() {
    let mut tree = Tree::new(Choices { choice: 0 }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let section = rect_of(&tree, "section");
    let next = rect_of(&tree, "next");
    assert_eq!(section.h, 88 * 2 + 100 + 2);
    assert_eq!(next.y, section.y + 89);
    assert_eq!((next.x, next.w), (section.x, section.w));
    assert_eq!(rect_of(&tree, "slider").w, section.w - 48);
    assert_eq!(text_rect(&tree, "Value").right() + 12 + 8 + 24, section.right());
}

struct Dividers;

impl Screen for Dividers {}

impl Component for Dividers {
    type Msg = ();

    fn update(&mut self, _msg: (), _cx: &mut Cx<Self>) {}

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        let vertical = widgets::vertical_divider(ui).id("vertical").part(Part::Line, |line| line.id("vertical-line"));
        ui.column([
            widgets::divider(ui).id("full"),
            widgets::divider(ui).padding_x(24).id("inset").part(Part::Line, |line| line.id("inset-line")),
            ui.row([ui.text(ui.theme().fonts.body, "A"), vertical, ui.text(ui.theme().fonts.body, "B")]).height(88).align(Align::Center),
        ])
        .align(Align::Center)
    }
}

#[test]
fn dividers_fill_a_centering_parent_across_their_line() {
    let mut tree = Tree::new(Dividers, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    assert_eq!(rect_of(&tree, "full"), Rect::new(0, 0, SCREEN.w, 1));
    let inset = rect_of(&tree, "inset-line");
    assert_eq!((inset.x, inset.w, inset.h), (24, SCREEN.w - 48, 1));
    let (vertical, line) = (rect_of(&tree, "vertical"), rect_of(&tree, "vertical-line"));
    assert_eq!((vertical.w, vertical.h), (1, 88));
    assert_eq!((line.y, line.w, line.h), (vertical.y + 12, 1, 88 - 24));
}

struct Dropdowns {
    chosen: &'static AtomicU32,
}

impl Screen for Dropdowns {}

impl Component for Dropdowns {
    type Msg = DropdownMsg;

    fn update(&mut self, msg: DropdownMsg, cx: &mut Cx<Self>) {
        match msg {
            DropdownMsg::Open => cx.popup(widgets::DropdownMenu::new(cx.tap_rect().unwrap(), &["One", "Two"], 1, cx.handle(), DropdownMsg::Chosen)),
            DropdownMsg::Chosen(index) => self.chosen.store(index as u32 + 1, Relaxed),
            DropdownMsg::Modal => cx.nav_of::<Overlay>().push(Dropdowns { chosen: self.chosen }),
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<DropdownMsg>) -> Element<'a, DropdownMsg> {
        let content = ui.column([ui.column([]).height(500), widgets::dropdown(ui, "One", DropdownMsg::Open).id("open"), ui.column([]).height(1000)]);
        ui.column([ui.row([]).size(40, 40).id("modal").on_tap(DropdownMsg::Modal), ui.scroll(content).grow(1)])
    }
}

#[derive(Clone, Copy)]
enum DropdownMsg {
    Open,
    Chosen(usize),
    Modal,
}

fn dropdown_tree() -> (Tree, &'static AtomicU32) {
    let chosen = Box::leak(Box::new(AtomicU32::new(0)));
    let mut tree = Tree::new(Dropdowns { chosen }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    (tree, chosen)
}

struct Menus {
    chosen: &'static AtomicU32,
}

impl Screen for Menus {}

impl Component for Menus {
    type Msg = MenuMsg;

    fn update(&mut self, msg: MenuMsg, cx: &mut Cx<Self>) {
        match msg {
            MenuMsg::Open => cx.popup(widgets::Menu::new(cx.tap_rect().unwrap(), &[("One", MenuMsg::Chosen(1)), ("Two", MenuMsg::Chosen(2))], cx.handle())),
            MenuMsg::Forward => cx.send(MenuMsg::Open),
            MenuMsg::Chosen(value) => {
                assert!(cx.tap_rect().is_none());
                self.chosen.store(value, Relaxed);
            }
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<MenuMsg>) -> Element<'a, MenuMsg> {
        ui.column([
            ui.row([]).size(60, 60).absolute(Some(20), Some(20), None, None).id("left").on_tap(MenuMsg::Open),
            ui.row([]).size(60, 60).absolute(None, Some(20), Some(20), None).id("right").on_tap(MenuMsg::Open),
            ui.row([]).size(60, 60).absolute(Some(20), None, None, Some(20)).id("bottom").on_tap(MenuMsg::Forward),
        ])
    }
}

#[derive(Clone, Copy)]
enum MenuMsg {
    Open,
    Forward,
    Chosen(u32),
}

fn menu_tree() -> (Tree, &'static AtomicU32) {
    let chosen = Box::leak(Box::new(AtomicU32::new(0)));
    let mut tree = Tree::new(Menus { chosen }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    (tree, chosen)
}

#[test]
fn a_menu_opens_under_its_anchor_from_the_left_edge() {
    let (mut tree, _) = menu_tree();
    tap(&mut tree, "left");
    let (left, menu) = (rect_of(&tree, "left"), rect_of(&tree, "menu"));
    assert_eq!((menu.x, menu.y), (left.x, left.bottom()));
    assert_eq!((menu.w, menu.h), (200, 116));
}

#[test]
fn a_menu_keeps_to_the_right_edge_of_its_anchor_where_it_would_not_fit() {
    let (mut tree, _) = menu_tree();
    tap(&mut tree, "right");
    let (right, menu) = (rect_of(&tree, "right"), rect_of(&tree, "menu"));
    assert_eq!((menu.right(), menu.y), (right.right(), right.bottom()));
}

#[test]
fn a_menu_opens_above_its_anchor_at_the_bottom_with_the_tap_through_a_send() {
    let (mut tree, _) = menu_tree();
    tap(&mut tree, "bottom");
    let (bottom, menu) = (rect_of(&tree, "bottom"), rect_of(&tree, "menu"));
    assert_eq!((menu.x, menu.bottom()), (bottom.x, bottom.y));
}

#[test]
fn a_menu_item_reports_to_the_target_and_closes_the_menu() {
    let (mut tree, chosen) = menu_tree();
    tap(&mut tree, "left");
    let menu = rect_of(&tree, "menu");
    tree.touch(&[(0, (menu.x + 10, menu.bottom() - 20))], 20);
    tree.touch(&[], 30);
    run(&mut tree);
    assert_eq!(chosen.load(Relaxed), 2);
    assert!(tree.find("menu").is_none());
    tap(&mut tree, "left");
    tap_at(&mut tree, (SCREEN.w - 10, SCREEN.h - 10));
    assert!(tree.find("menu").is_none());
    assert_eq!(chosen.load(Relaxed), 2);
}

#[test]
fn a_dropdown_menu_touches_the_dropdown_and_opens_upward_at_the_bottom() {
    let (mut tree, _) = dropdown_tree();
    tap(&mut tree, "open");
    let open = rect_of(&tree, "open");
    assert_eq!(rect_of(&tree, "dropdown-menu").bottom(), open.y);
    assert_eq!(rect_of(&tree, "dropdown-menu").x, open.x);
    tap_at(&mut tree, (SCREEN.w - 10, 10));
    assert!(tree.find("dropdown-menu").is_none());
    tree.touch(&[(0, (100, 300))], 0);
    tree.touch(&[(0, (100, 0))], 16);
    tree.touch(&[], 32);
    run(&mut tree);
    tap(&mut tree, "open");
    let open = rect_of(&tree, "open");
    assert_eq!(rect_of(&tree, "dropdown-menu").y, open.bottom());
    assert_eq!(rect_of(&tree, "dropdown-menu").w, open.w);
    let selected = |tree: &Tree, label: &str| {
        let found = tree.nodes.iter().filter(|node| matches!(&node.content, NodeContent::Text(text) if text.text() == label)).last().unwrap();
        tree.nodes[found.parent as usize].state.contains(State::SELECTED)
    };
    assert!(selected(&tree, "Two"));
    assert!(!selected(&tree, "One"));
}

#[test]
fn a_popup_stays_over_a_modal_and_reports_to_it() {
    let (mut tree, chosen) = dropdown_tree();
    tap(&mut tree, "modal");
    settle(&mut tree);
    let shown = |tree: &Tree| {
        let mut count = 0;
        tree.walk(tree.root, &mut |_, node| count += u32::from(node.id == Some("open")));
        count
    };
    assert_eq!(shown(&tree), 2, "the modal over the first screen");
    let modal = tree.nodes[tree.popups as usize].first_child;
    assert_eq!(modal, NONE);
    let overlay = tree.navigators[tree.navigators.len() - 1];
    let open = tree.nodes[tree.nodes[overlay as usize].first_child as usize].screen;
    let point = (open.x + 10, 500 + 40 + 10);
    tree.touch(&[(0, point)], 0);
    tree.touch(&[], 10);
    tree.update();
    assert!(tree.find("dropdown-menu").is_some());
    assert_eq!(shown(&tree), 2, "still there under the popup");
    let menu = rect_of(&tree, "dropdown-menu");
    tree.touch(&[(0, (menu.x + 10, menu.bottom() - 10))], 20);
    tree.touch(&[], 30);
    settle(&mut tree);
    assert!(tree.find("dropdown-menu").is_none());
    assert_eq!(chosen.load(Relaxed), 2);
}

#[test]
fn turning_the_screen_closes_popups() {
    let (mut tree, _) = dropdown_tree();
    tap(&mut tree, "open");
    assert!(tree.find("dropdown-menu").is_some());
    tree.set_area(Rect::new(0, 0, 600, 400));
    run(&mut tree);
    assert!(tree.find("dropdown-menu").is_none());
}

#[test]
fn popups_stay_above_the_keyboard() {
    let (mut tree, _) = dropdown_tree();
    tree.add_keyboard(crate::keyboard::KeyboardMode::Auto);
    let mut last = tree.nodes[tree.root as usize].first_child;
    while tree.nodes[last as usize].next != NONE {
        last = tree.nodes[last as usize].next;
    }
    assert_eq!(last, tree.popups);
}

struct Scroller {
    offsets: &'static core::cell::RefCell<Vec<i32>>,
}

impl Screen for Scroller {}

impl Component for Scroller {
    type Msg = i32;

    fn update(&mut self, msg: i32, _cx: &mut Cx<Self>) {
        self.offsets.borrow_mut().push(msg);
    }

    fn view<'a>(&'a self, ui: &'a Ui<i32>) -> Element<'a, i32> {
        ui.column([
            ui.column([]).height(80).scroll_border("content").id("bar"),
            ui.scroll(ui.column([ui.column([]).height(2000)])).grow(1).on_scroll(|offset| offset).id("content"),
        ])
    }
}

#[test]
fn a_scroll_reports_every_move_and_shows_the_border_only_away_from_the_start() {
    let offsets = Box::leak(Box::new(core::cell::RefCell::new(Vec::new())));
    let mut tree = Tree::new(Scroller { offsets }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let bar = rect_of(&tree, "bar");
    let owner = tree.nodes[tree.find("bar").unwrap() as usize].owner;
    let drag = |tree: &mut Tree, y: i16, at: u32| {
        tree.touch(&[(0, (100, y))], at);
        tree.update();
        tree.take_damage()
    };
    drag(&mut tree, 300, 0);
    let first = drag(&mut tree, 260, 16);
    assert!(tree.scrolled_away(owner, "content"));
    assert!(first.iter().any(|r| r.intersect(&bar).h > 0), "{first:?}");
    let second = drag(&mut tree, 240, 32);
    assert!(second.iter().all(|r| r.intersect(&bar).h == 0), "{second:?}");
    assert_eq!(offsets.borrow().len(), 2);
    let back = drag(&mut tree, 600, 48);
    assert!(!tree.scrolled_away(owner, "content"));
    assert!(back.iter().any(|r| r.intersect(&bar).h > 0), "{back:?}");
    assert!(*offsets.borrow().last().unwrap() < 0, "past the start, as it shows");
    tree.touch(&[], 64);
}

#[test]
fn a_clip_with_a_radius_is_round_and_draws_its_border_over_what_is_inside() {
    let (mut tree, _) = dropdown_tree();
    tap(&mut tree, "open");
    run(&mut tree);
    let mut effects = crate::paint::Effects::new(Caps::INTERNAL);
    let mut list = devkit_gfx::DisplayList::new_in(Caps::INTERNAL);
    tree.paint(&mut effects, &[SCREEN], &mut list);
    let ops = list.ops();
    let push = ops.iter().position(|op| matches!(op, devkit_gfx::Op::PushRoundClip { .. })).unwrap();
    let pop = ops.iter().position(|op| matches!(op, devkit_gfx::Op::PopRoundClip)).unwrap();
    assert!(matches!(ops[pop + 1], devkit_gfx::Op::Border { .. }));
    assert!(ops[push..pop].iter().any(|op| matches!(op, devkit_gfx::Op::Text { .. })));
}

#[test]
fn tap_runs_the_handler_and_changes_only_what_changed() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let id = tree.find("count").unwrap();
    tap(&mut tree, "button");
    let NodeContent::Text(text) = &tree.nodes[id as usize].content else { panic!() };
    assert_eq!(text.text(), "n=1");
    let damage = tree.take_damage();
    assert!(damage.iter().all(|r| r.bottom() <= 40), "{damage:?}");
    assert!(!damage.is_empty());
}

#[test]
fn press_needs_no_rebuild() {
    let sample = Sample::new();
    let views = sample.views;
    let mut tree = Tree::new(sample, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let before = views.get();
    let point = tree.tap_point("button").unwrap();
    tree.touch(&[(0, point)], 0);
    let damage = run(&mut tree);
    assert_eq!(views.get(), before);
    assert!(damage.contains(&Rect::new(350, 5, 40, 30)));
}

#[test]
fn keys_keep_nodes_when_siblings_move() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let (a, b) = (tree.find("a").unwrap(), tree.find("b").unwrap());
    tree.post(tree.root_id, crate::erased::Erased::new(Msg::Reorder));
    run(&mut tree);
    assert_eq!(tree.find("a"), Some(a));
    assert_eq!(tree.find("b"), Some(b));
    assert!(rect_of(&tree, "b").x < rect_of(&tree, "a").x);
}

#[test]
fn removed_nodes_damage_their_area_and_are_reused() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    tree.post(tree.root_id, crate::erased::Erased::new(Msg::Toggle));
    run(&mut tree);
    let panel = rect_of(&tree, "panel");
    assert_eq!(panel, Rect::new(290, 20, 100, 100));
    let nodes = tree.nodes.len();
    tree.post(tree.root_id, crate::erased::Erased::new(Msg::Toggle));
    let damage = run(&mut tree);
    assert!(damage.contains(&panel));
    assert!(tree.find("panel").is_none());
    tree.post(tree.root_id, crate::erased::Erased::new(Msg::Toggle));
    run(&mut tree);
    assert_eq!(tree.nodes.len(), nodes);
}

#[test]
fn child_state_survives_and_events_reach_the_parent() {
    let sample = Sample::new();
    let views = sample.views;
    let mut tree = Tree::new(sample, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let before = views.get();
    tap(&mut tree, "counter");
    assert_eq!(views.get(), before + 1, "the event rebuilt the parent");
    assert_eq!(rect_of(&tree, "counter").h, 21);
    let NodeContent::Text(text) = &tree.nodes[tree.find("count").unwrap() as usize].content else { panic!() };
    assert_eq!(text.text(), "n=10");
    tap(&mut tree, "button");
    assert_eq!(rect_of(&tree, "counter").h, 21, "same props keep the child's state");
}

#[test]
fn list_builds_only_visible_rows_and_keeps_them_while_scrolling() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let list_id = tree.find("list").unwrap();
    let rows = |tree: &Tree| match &tree.nodes[list_id as usize].content {
        NodeContent::List(list) => list.rows.iter().map(|r| r.0).collect::<Vec<_>>(),
        _ => panic!(),
    };
    let viewport = rect_of(&tree, "list");
    assert!(rows(&tree).len() <= viewport.h as usize / 50 + 3);
    let nodes = tree.nodes.len();
    let x = viewport.x + 10;
    tree.touch(&[(0, (x, viewport.y + 200))], 0);
    tree.touch(&[(0, (x, viewport.y + 100))], 16);
    tree.touch(&[(0, (x, viewport.y + 20))], 32);
    let damage = run(&mut tree);
    assert!(damage.iter().all(|r| *r == viewport));
    let shown = rows(&tree);
    assert!(shown[0] > 0);
    assert!(tree.nodes.len() <= nodes + 8);
    tree.touch(&[], 40);
    assert!(tree.animating());
    tree.tick(16);
    assert!(tree.animating());
    for _ in 0..400 {
        tree.tick(16);
    }
    assert!(!tree.animating());
    run(&mut tree);
    assert!(rows(&tree)[0] > shown[0]);
}

#[test]
fn a_new_string_of_the_same_box_skips_layout_and_a_revision_keeps_rows() {
    #[derive(Clone, Copy)]
    struct Set(u8);
    struct Label {
        font: &'static FontChain,
        value: u8,
        revision: u32,
        rows: &'static core::cell::Cell<u32>,
    }
    impl Screen for Label {}
    impl Component for Label {
        type Msg = Set;
        fn update(&mut self, msg: Set, cx: &mut Cx<Self>) {
            self.value = msg.0;
            cx.rebuild();
        }
        fn view<'a>(&'a self, ui: &'a Ui<Set>) -> Element<'a, Set> {
            ui.column([
                ui.row([ui.text(self.font, ui.format(format_args!("{}", "x".repeat(usize::from(self.value))))).grow(1).id("label")]),
                ui.list(0, 100, 50).grow(1).revision(self.revision),
            ])
        }
        fn list_row<'a>(&'a self, _: ListId, _: usize, ui: &'a Ui<Set>) -> Element<'a, Set> {
            self.rows.set(self.rows.get() + 1);
            ui.column([])
        }
    }
    let rows = Box::leak(Box::new(core::cell::Cell::new(0)));
    let mut tree = Tree::new(Label { font: chain(), value: 2, revision: 0, rows }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let built = rows.get();
    assert!(built > 0);
    tree.post(tree.root_id, crate::erased::Erased::new(Set(9)));
    let damage = run(&mut tree);
    assert_eq!(rows.get(), built);
    assert!(damage.iter().all(|r| r.h <= 12), "{damage:?}");
    let NodeContent::Text(text) = &tree.nodes[tree.find("label").unwrap() as usize].content else { panic!() };
    assert_eq!(text.run.width(), 72);
}

#[test]
fn a_fling_survives_a_slow_last_sample_but_not_a_pause() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let viewport = rect_of(&tree, "list");
    let x = viewport.x + 10;
    let drag = |tree: &mut Tree, last_ms: u32, up_ms: u32| {
        tree.touch(&[(0, (x, viewport.y + 300))], 0);
        tree.touch(&[(0, (x, viewport.y + 260))], 10);
        tree.touch(&[(0, (x, viewport.y + 220))], 20);
        tree.touch(&[(0, (x, viewport.y + 180))], 30);
        tree.touch(&[(0, (x, viewport.y + 178))], last_ms);
        tree.touch(&[(0, (x, viewport.y + 178))], last_ms + 10);
        tree.touch(&[], up_ms);
    };
    drag(&mut tree, 40, 55);
    assert!(tree.animating(), "the last slow samples do not cancel the throw");
    for _ in 0..400 {
        tree.tick(16);
    }
    run(&mut tree);
    drag(&mut tree, 40, 120);
    assert!(!tree.animating(), "resting before lifting throws nothing");
}

#[test]
fn repeated_samples_leave_the_speed_alone_and_a_short_tick_keeps_the_fling() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let viewport = rect_of(&tree, "list");
    let x = viewport.x + 10;
    tree.touch(&[(0, (x, viewport.y + 300))], 0);
    tree.touch(&[(0, (x, viewport.y + 260))], 10);
    tree.touch(&[(0, (x, viewport.y + 220))], 20);
    tree.touch(&[(0, (x, viewport.y + 220))], 30);
    tree.touch(&[(0, (x, viewport.y + 220))], 40);
    tree.touch(&[], 45);
    let offset = |tree: &Tree| match &tree.nodes[tree.find("list").unwrap() as usize].content {
        NodeContent::List(list) => list.offset,
        _ => panic!(),
    };
    tree.tick(0);
    tree.tick(0);
    assert!(tree.animating(), "a tick too short to move keeps going");
    let before = offset(&tree);
    tree.tick(10);
    assert!(offset(&tree) - before >= 30, "still at the drag's 4 px/ms");
}

#[test]
fn drag_cancels_the_tap_of_a_row() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let point = tree.tap_point("row2").unwrap();
    tree.touch(&[(0, point)], 0);
    tree.touch(&[(0, (point.0, point.1 - 40))], 16);
    tree.touch(&[], 500);
    tree.update();
    assert!(tree.find("panel").is_none());
}

#[test]
fn covered_nodes_refuse_harness_taps_and_invoke_goes_direct() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    tree.invoke("row2").unwrap();
    run(&mut tree);
    assert!(tree.find("panel").is_some());
    assert!(tree.tap_point("button").is_err() == (rect_of(&tree, "panel").intersects(&rect_of(&tree, "button"))));
    assert!(tree.tap_point("nothing").is_err());
    assert!(tree.describe().contains("button button") || tree.describe().contains("button box"));
}

#[test]
fn grid_and_absolute_inset() {
    #[derive(Clone, Copy)]
    enum Nothing {}
    struct Grid(&'static FontChain);
    impl Screen for Grid {}
    impl Component for Grid {
        type Msg = Nothing;
        fn update(&mut self, _: Nothing, _: &mut Cx<Self>) {}
        fn view<'a>(&'a self, ui: &'a Ui<Nothing>) -> Element<'a, Nothing> {
            ui.column([
                ui.grid(3, [
                    ui.column([]).height(10).id("c0"),
                    ui.column([]).height(20).id("c1"),
                    ui.column([]).height(10).id("c2"),
                    ui.column([]).aspect_ratio(1, 1).id("c3"),
                ])
                .gap(10)
                .padding_x(5),
                ui.text(self.0, "ABCDEFGHIJ").width(50).ellipsis().id("clipped"),
                ui.column([]).absolute(Some(0), Some(0), Some(0), Some(0)).id("cover"),
            ])
        }
    }
    let mut tree = Tree::new(Grid(chain()), Rect::new(0, 0, 320, 200), Caps::INTERNAL, theme());
    run(&mut tree);
    assert_eq!(rect_of(&tree, "c1"), Rect::new(5 + 106, 0, 96, 20));
    assert_eq!(rect_of(&tree, "c3"), Rect::new(5, 30, 96, 96));
    assert_eq!(rect_of(&tree, "clipped").w, 50);
    assert_eq!(rect_of(&tree, "cover"), Rect::new(0, 0, 320, 200));
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum PageMsg {
    Open,
    OpenRoot,
    Back,
    Nothing,
    Slide(u8),
}

struct Page {
    name: &'static str,
    font: &'static FontChain,
    log: &'static core::cell::RefCell<Vec<(&'static str, &'static str)>>,
    opens: u32,
    value: u8,
}

impl Page {
    fn new(name: &'static str, log: &'static core::cell::RefCell<Vec<(&'static str, &'static str)>>) -> Self {
        Self { name, font: chain(), log, opens: 0, value: 0 }
    }

    fn note(&self, event: &'static str) {
        self.log.borrow_mut().push((self.name, event));
    }
}

impl Component for Page {
    type Msg = PageMsg;

    fn update(&mut self, msg: PageMsg, cx: &mut Cx<Self>) {
        match msg {
            PageMsg::Open => {
                self.opens += 1;
                cx.nav_of::<Pane>().push(Page::new("detail", self.log));
            }
            PageMsg::OpenRoot => cx.nav_of::<Root>().push(Page::new("full", self.log)),
            PageMsg::Back => cx.nav().pop(),
            PageMsg::Slide(value) => self.value = value,
            PageMsg::Nothing => {}
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<PageMsg>) -> Element<'a, PageMsg> {
        ui.column([
            ui.text(self.font, ui.format(format_args!("{} {}", self.name, self.opens))).id(self.name),
            ui.row([]).size(40, 40).id("open").on_tap(PageMsg::Open),
            ui.row([]).size(40, 40).id("open-root").on_tap(PageMsg::OpenRoot),
            ui.row([]).size(40, 40).id("back").on_tap(PageMsg::Back),
            widgets::slider(ui, self.value, |d| PageMsg::Slide(widgets::slider_value(d))).width(200).id("slider"),
            ui.list(0, 100, 50).id("rows").grow(1),
        ])
    }

    fn list_row<'a>(&'a self, _: ListId, _: usize, ui: &'a Ui<PageMsg>) -> Element<'a, PageMsg> {
        ui.column([]).height(50).on_tap(PageMsg::Nothing)
    }
}

impl Screen for Page {
    fn on_enter(&mut self, _: &mut Cx<Self>) {
        self.note("enter");
    }
    fn on_exit(&mut self, _: &mut Cx<Self>) {
        self.note("exit");
    }
    fn on_appear(&mut self, _: &mut Cx<Self>) {
        self.note("appear");
    }
    fn on_disappear(&mut self, _: &mut Cx<Self>) {
        self.note("disappear");
    }
}

impl Child for Page {
    fn create(_: &()) -> Self {
        Page::new("pane", SPLIT_LOG.with(|l| *l))
    }
    fn set_props(&mut self, _: &()) {}
}

struct Pane;

std::thread_local! {
    static SPLIT_LOG: &'static core::cell::RefCell<Vec<(&'static str, &'static str)>> = Box::leak(Box::new(core::cell::RefCell::new(Vec::new())));
}

struct Split {
    page: Page,
}

impl Component for Split {
    type Msg = PageMsg;

    fn update(&mut self, msg: PageMsg, cx: &mut Cx<Self>) {
        self.page.update(msg, unsafe { &mut *(cx as *mut Cx<Self>).cast::<Cx<Page>>() });
    }

    fn view<'a>(&'a self, ui: &'a Ui<PageMsg>) -> Element<'a, PageMsg> {
        ui.row([
            ui.column([ui.row([]).size(40, 40).id("side-open").on_tap(PageMsg::Open)]).width(100),
            ui.navigator::<Pane, Page>(()).grow(1).id("pane-nav"),
        ])
    }
}

impl Screen for Split {}

fn settle(tree: &mut Tree) {
    for _ in 0..40 {
        tree.update();
        tree.tick(16);
    }
    tree.update();
    tree.take_damage();
}

#[test]
fn a_pane_navigates_inside_its_area_and_the_root_over_everything() {
    let log = SPLIT_LOG.with(|l| *l);
    let mut tree = Tree::new(Split { page: Page::new("split", log) }, SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    assert_eq!(rect_of(&tree, "pane").x, 100);

    tap(&mut tree, "side-open");
    assert!(tree.animating());
    tree.tick(16);
    let damage = run(&mut tree);
    assert!(damage.iter().all(|r| r.is_empty() || r.x >= 100), "a pane transition stays in the pane: {damage:?}");
    settle(&mut tree);
    assert_eq!(rect_of(&tree, "detail").x, 100);
    assert!(tree.find("pane").is_none(), "the covered screen has no nodes");

    tap(&mut tree, "back");
    settle(&mut tree);
    assert!(tree.find("pane").is_some());
    assert!(tree.find("detail").is_none());
    assert!(log.borrow().contains(&("detail", "exit")));

    tap(&mut tree, "open-root");
    settle(&mut tree);
    assert_eq!(rect_of(&tree, "full").x, 0);
    assert!(tree.find("side-open").is_none());
    tap(&mut tree, "back");
    settle(&mut tree);
    assert!(tree.find("side-open").is_some());
    assert!(tree.find("pane").is_some(), "the pane comes back with the screen around it");
}

#[test]
fn state_and_scroll_position_come_back_and_taps_wait_for_the_slide() {
    let log = Box::leak(Box::new(core::cell::RefCell::new(Vec::new())));
    let mut tree = Tree::new(PageRoot(Page::new("first", log)), SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    let list = rect_of(&tree, "rows");
    let x = list.x + 10;
    tree.touch(&[(0, (x, list.y + 200))], 0);
    tree.touch(&[(0, (x, list.y + 100))], 500);
    tree.touch(&[(0, (x, list.y + 50))], 1000);
    tree.touch(&[], 2000);
    settle(&mut tree);
    let offset = |tree: &Tree, name: &str| match &tree.nodes[tree.find(name).unwrap() as usize].content {
        NodeContent::List(list) => list.offset,
        _ => panic!(),
    };
    let before = offset(&tree, "rows");
    assert!(before > 0);

    tree.invoke("open-root").unwrap();
    tree.update();
    assert!(tree.tap_point("open").is_err(), "taps wait for the slide");
    settle(&mut tree);
    tap(&mut tree, "back");
    settle(&mut tree);
    assert_eq!(offset(&tree, "rows"), before);
    let NodeContent::Text(text) = &tree.nodes[tree.find("first").unwrap() as usize].content else { panic!() };
    assert_eq!(text.text(), "first 0");
    let events: Vec<_> = log.borrow().iter().filter(|e| e.0 == "first").map(|e| e.1).collect();
    assert_eq!(events, ["enter", "appear", "disappear", "appear"]);
}

struct PageRoot(Page);

impl Component for PageRoot {
    type Msg = PageMsg;
    fn update(&mut self, msg: PageMsg, cx: &mut Cx<Self>) {
        self.0.update(msg, unsafe { &mut *(cx as *mut Cx<Self>).cast::<Cx<Page>>() });
    }
    fn view<'a>(&'a self, ui: &'a Ui<PageMsg>) -> Element<'a, PageMsg> {
        self.0.view(ui)
    }
    fn list_row<'a>(&'a self, list: ListId, index: usize, ui: &'a Ui<PageMsg>) -> Element<'a, PageMsg> {
        self.0.list_row(list, index, ui)
    }
}

impl Screen for PageRoot {
    fn on_enter(&mut self, _: &mut Cx<Self>) {
        self.0.note("enter");
    }
    fn on_appear(&mut self, _: &mut Cx<Self>) {
        self.0.note("appear");
    }
    fn on_disappear(&mut self, _: &mut Cx<Self>) {
        self.0.note("disappear");
    }
}

fn slider_nodes(tree: &Tree) -> [NodeId; 3] {
    let bar = tree.nodes[tree.find("slider").unwrap() as usize].first_child;
    let track = tree.nodes[bar as usize].first_child;
    let stop = tree.nodes[track as usize].first_child;
    [track, stop, tree.nodes[stop as usize].first_child]
}

fn slider_thumb(tree: &Tree, fill: NodeId) -> Rect {
    let end = tree.nodes[fill as usize].first_child;
    tree.nodes[tree.nodes[end as usize].first_child as usize].screen
}

#[test]
fn a_drag_follows_the_contact_without_a_tap_slop() {
    let log = Box::leak(Box::new(core::cell::RefCell::new(Vec::new())));
    let mut tree = Tree::new(PageRoot(Page::new("first", log)), SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    let slider = rect_of(&tree, "slider");
    tree.touch(&[(0, (slider.x + 100, slider.y + 10))], 0);
    tree.update();
    let [_, stop, fill] = slider_nodes(&tree);
    assert_eq!(tree.nodes[stop as usize].rect.w, 85);
    tree.touch(&[(0, (slider.x + 150, slider.y + 10))], 16);
    tree.update();
    assert_eq!(tree.nodes[stop as usize].rect.w, 135);
    let thumb = slider_thumb(&tree, fill);
    assert_eq!(thumb.x + thumb.w / 2, tree.nodes[fill as usize].screen.right());
    tree.touch(&[], 32);
    tree.update();
}

#[test]
fn the_track_keeps_its_inset_and_the_thumb_stays_on_the_slider() {
    let log = Box::leak(Box::new(core::cell::RefCell::new(Vec::new())));
    let mut tree = Tree::new(PageRoot(Page::new("first", log)), SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    let slider = rect_of(&tree, "slider");
    let [track, _, fill] = slider_nodes(&tree);
    let track = tree.nodes[track as usize].screen;
    assert_eq!((track.x, track.w), (slider.x + 4, slider.w - 8));
    tree.touch(&[(0, (slider.x, slider.y + 10))], 0);
    tree.update();
    assert_eq!(slider_thumb(&tree, fill).x, slider.x);
    assert_eq!(tree.nodes[fill as usize].screen.x, track.x);
    tree.touch(&[(0, (slider.right() - 1, slider.y + 10))], 16);
    tree.update();
    assert_eq!(slider_thumb(&tree, fill).right(), slider.right());
    tree.touch(&[], 32);
    tree.update();
}

struct Parts;

impl Screen for Parts {}

impl Component for Parts {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut Cx<Self>) {}

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        ui.column([
            widgets::slider(ui, 0, |_| ()).part(Part::Thumb, |thumb| thumb.background(RED).id("thumb")).part(Part::Fill, |fill| fill.id("fill")),
            widgets::switch(ui, false, ()).part(Part::Track, |track| track.background(RED).id("track")),
            widgets::list_item(ui, None, "Title", None, None, ()).part(Part::Meta, |meta| meta.id("no-meta")).part(Part::Title, |title| title.foreground(RED).id("title")),
            widgets::segmented(ui, &[("A", ()), ("B", ())], 9).part(Part::Item, |item| item.background(WHITE)).part(Part::Label, |label| label.font(ui.theme().fonts.title).id("label")).id("segments"),
            ui.row([widgets::slider(ui, 0, |_| ()).width(200)]).part(Part::Thumb, |thumb| thumb.id("nested-thumb")),
        ])
    }
}

#[test]
fn a_part_reaches_the_pieces_of_a_widget_inside_it() {
    let mut tree = Tree::new(Parts, SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    let background = |id: &str| tree.nodes[tree.find(id).unwrap() as usize].style.background;
    assert_eq!(background("thumb"), Some(RED.into()));
    assert!(background("fill").is_some(), "the rest of the widget stays");
    assert_eq!(background("track"), Some(RED.into()), "the root can be a part");
    assert!(tree.find("title").is_some());
    assert!(tree.find("no-meta").is_none(), "an absent part does nothing");
    assert!(tree.find("nested-thumb").is_some());
    assert!(tree.find("label").is_some());

    let mut item = tree.nodes[tree.find("segments").unwrap() as usize].first_child;
    let mut items = 0;
    while item != NONE {
        assert_eq!(tree.nodes[item as usize].style.background, Some(WHITE.into()));
        item = tree.nodes[item as usize].next;
        items += 1;
    }
    assert_eq!(items, 2, "every item is the part");
}

#[test]
fn an_animation_moves_a_node_without_a_rebuild() {
    let sample = Sample::new();
    let views = sample.views;
    let mut tree = Tree::new(sample, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let before = views.get();
    let id = tree.find("button").unwrap();
    tree.dispatch_animation("button", Animation { property: Property::TranslateX, from: -40, to: 0, duration_ms: 100 });
    let damage = run(&mut tree);
    assert_eq!(tree.nodes[id as usize].translate.0, -40);
    assert!(!damage.is_empty());
    assert!(tree.animating());
    tree.tick(50);
    let middle = tree.nodes[id as usize].translate.0;
    assert!((-10..0).contains(&middle), "{middle}");
    tree.tick(50);
    assert_eq!(tree.nodes[id as usize].translate.0, 0);
    assert!(!tree.animating());
    run(&mut tree);
    assert_eq!(views.get(), before);
}

static EASED_TRACK: StateStyle = StateStyle {
    font: None,
    base: &[Prop::Width(64), Prop::Height(32), Prop::Background(ColorRole::Fixed(RED)), Prop::Animate(Animated::COLOR, 100)],
    states: &[
        (State::CHECKED, &[Prop::Background(ColorRole::Fixed(BLUE))]),
        (State::PRESSED, &[Prop::Background(ColorRole::Fixed(WHITE)), Prop::Animate(Animated::empty(), 0)]),
    ],
};
static EASED_THUMB: StateStyle = StateStyle { base: &[Prop::Width(24), Prop::Height(24), Prop::Animate(Animated::POSITION, 100)], ..StateStyle::EMPTY };
static FADING: StateStyle = StateStyle {
    font: None,
    base: &[Prop::Width(24), Prop::Height(24), Prop::Animate(Animated::OPACITY, 100)],
    states: &[(State::CHECKED, &[Prop::Opacity(0)])],
};

struct Toggled {
    on: bool,
    views: &'static core::cell::Cell<u32>,
}

impl Screen for Toggled {}

impl Component for Toggled {
    type Msg = ();

    fn update(&mut self, _: (), cx: &mut Cx<Self>) {
        self.on = !self.on;
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        self.views.set(self.views.get() + 1);
        let thumb = ui.column([]).style(&EASED_THUMB).id("thumb");
        let track = ui.row([thumb]).style(&EASED_TRACK).justify(if self.on { Justify::End } else { Justify::Start }).checked(self.on).id("track").on_tap(());
        ui.column([track, ui.column([]).style(&FADING).checked(self.on).id("fading")])
    }
}

fn toggled() -> (Tree, &'static core::cell::Cell<u32>) {
    let views: &'static core::cell::Cell<u32> = Box::leak(Box::new(core::cell::Cell::new(0)));
    let mut tree = Tree::new(Toggled { on: false, views }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    (tree, views)
}

fn fill_of(tree: &Tree, name: &str) -> Option<Color> {
    let node = &tree.nodes[tree.find(name).unwrap() as usize];
    let palette = tree.theme.palette(tree.color_mode);
    crate::animate::faded(node.fades.as_ref().and_then(|f| f.background.as_ref()), node.style.background.map(|role| palette.color(role)))
}

#[test]
fn a_style_eases_position_and_color_without_a_view_or_layout() {
    let (mut tree, views) = toggled();
    let (track, thumb) = (tree.find("track").unwrap(), tree.find("thumb").unwrap());
    assert_eq!(rect_of(&tree, "thumb").x, 0, "nothing eases into the first layout");
    assert!(!tree.animating());
    tree.post(tree.root_id, Erased::new(()));
    let before = rect_of(&tree, "thumb");
    run(&mut tree);
    assert_eq!(rect_of(&tree, "thumb"), before, "it starts from where it showed");
    assert_eq!(tree.nodes[thumb as usize].rect.x, 40);
    assert_eq!(fill_of(&tree, "track"), Some(RED));
    assert!(tree.animating());
    let built = views.get();
    tree.tick(50);
    let damage = run(&mut tree);
    let middle = rect_of(&tree, "thumb").x;
    assert!((1..40).contains(&middle), "{middle}");
    let mixed = fill_of(&tree, "track").unwrap();
    assert!(mixed.r > 0 && mixed.r < 255 && mixed.b > 0 && mixed.b < 255, "{mixed:?}");
    assert!(damage.contains(&tree.nodes[track as usize].screen));
    tree.tick(50);
    run(&mut tree);
    assert_eq!(rect_of(&tree, "thumb").x, 40);
    assert_eq!(fill_of(&tree, "track"), Some(BLUE));
    assert!(!tree.animating());
    assert!(tree.nodes[track as usize].fades.is_none());
    assert_eq!(views.get(), built);
}

#[test]
fn turning_back_midway_starts_from_what_shows() {
    let (mut tree, _) = toggled();
    tree.post(tree.root_id, Erased::new(()));
    run(&mut tree);
    tree.tick(30);
    run(&mut tree);
    let (x, color) = (rect_of(&tree, "thumb").x, fill_of(&tree, "track"));
    assert!(x > 0);
    tree.post(tree.root_id, Erased::new(()));
    run(&mut tree);
    assert_eq!(rect_of(&tree, "thumb").x, x);
    assert_eq!(fill_of(&tree, "track"), color);
    tree.tick(100);
    run(&mut tree);
    assert_eq!(rect_of(&tree, "thumb").x, 0);
    assert_eq!(fill_of(&tree, "track"), Some(RED));
}

#[test]
fn a_fade_started_by_a_touch_does_not_count_the_idle_time_before_it() {
    let (mut tree, _) = toggled();
    let point = tree.tap_point("track").unwrap();
    tree.touch(&[(0, point)], 0);
    run(&mut tree);
    assert_eq!(fill_of(&tree, "track"), Some(WHITE), "pressing eases nothing in this style");
    assert!(!tree.animating());
    tree.touch(&[], 10);
    tree.tick(3_600_000);
    run(&mut tree);
    assert_eq!(fill_of(&tree, "track"), Some(WHITE));
    assert!(tree.animating());
    tree.tick(100);
    run(&mut tree);
    assert_eq!(fill_of(&tree, "track"), Some(BLUE));
}

#[test]
fn opacity_eases_to_the_style_and_leaves_nothing_behind() {
    let (mut tree, _) = toggled();
    let id = tree.find("fading").unwrap();
    tree.post(tree.root_id, Erased::new(()));
    run(&mut tree);
    tree.tick(50);
    run(&mut tree);
    let shown = tree.nodes[id as usize].fades.as_ref().and_then(|f| f.opacity).map(|fade| fade.opacity(0)).unwrap();
    assert!((1..255).contains(&shown), "{shown}");
    tree.tick(50);
    run(&mut tree);
    assert!(tree.nodes[id as usize].fades.is_none());
    assert_eq!(tree.nodes[id as usize].opacity, None);
    assert_eq!(tree.nodes[id as usize].style.opacity, Some(0));
}

struct Loading {
    value: Option<f32>,
    shown: bool,
}

impl Screen for Loading {}

impl Component for Loading {
    type Msg = Option<f32>;

    fn update(&mut self, value: Option<f32>, cx: &mut Cx<Self>) {
        match value {
            Some(v) if v < 0.0 => self.shown = false,
            _ => self.value = value,
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Option<f32>>) -> Element<'a, Option<f32>> {
        if !self.shown {
            return ui.column([]);
        }
        let bar = widgets::progress_bar(ui, self.value).width(200).height(8).id("bar");
        let ring = widgets::progress_indicator(ui, self.value).width(48).height(48).id("ring");
        let thin = widgets::progress_indicator(ui, self.value).stroke(2).width(24).height(24).id("thin");
        ui.column([bar, ring, thin])
    }
}

fn phases(tree: &Tree) -> usize {
    tree.motions.iter().filter(|m| matches!(m.kind, crate::motion::MotionKind::Phase)).count()
}

#[test]
fn a_progress_not_knowing_how_far_moves_by_itself_and_stops_when_it_knows() {
    let mut tree = Tree::new(Loading { value: None, shown: true }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    assert_eq!(phases(&tree), 3);
    assert!(tree.animating());
    tree.tick(100);
    let damage = run(&mut tree);
    let ring = rect_of(&tree, "ring");
    assert!(damage.contains(&ring), "{damage:?}");
    assert!(damage.iter().all(|d| d.is_empty() || ["bar", "ring", "thin"].iter().any(|id| d.intersects(&rect_of(&tree, id)))), "only the progress repaints: {damage:?}");
    tree.post(tree.root_id, Erased::new(Some(0.5f32)));
    run(&mut tree);
    assert_eq!(phases(&tree), 0);
    assert!(!tree.animating());
    tree.post(tree.root_id, Erased::new(None::<f32>));
    run(&mut tree);
    assert_eq!(phases(&tree), 3);
    tree.post(tree.root_id, Erased::new(Some(-1.0f32)));
    run(&mut tree);
    assert_eq!(phases(&tree), 0, "a removed progress stops");
    assert!(!tree.animating());
}

fn with_ops(tree: &Tree, check: impl FnOnce(&[devkit_gfx::Op<'_>])) {
    let mut effects = crate::paint::Effects::new(Caps::INTERNAL);
    let mut list = devkit_gfx::DisplayList::new_in(Caps::INTERNAL);
    tree.paint(&mut effects, &[SCREEN], &mut list);
    check(list.ops());
}

#[test]
fn a_progress_paints_a_track_and_how_far() {
    let mut tree = Tree::new(Loading { value: Some(0.5), shown: true }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let (bar, ring) = (rect_of(&tree, "bar"), rect_of(&tree, "ring"));
    with_ops(&tree, |ops| {
        assert!(ops.iter().any(|op| matches!(op, devkit_gfx::Op::Fill { rect, .. } if rect.right() == bar.x + 100 && rect.w == 200)), "the fill ends halfway");
        let sweeps: Vec<f32> = ops.iter().filter_map(|op| match op {
            devkit_gfx::Op::Arc { rect, sweep, .. } if *rect == ring => Some(*sweep),
            _ => None,
        }).collect();
        assert_eq!(sweeps, [360.0, 180.0]);
        let thin = rect_of(&tree, "thin");
        let masks: Vec<(u16, u16)> = ops.iter().filter_map(|op| match op {
            devkit_gfx::Op::Arc { rect, mask, .. } if *rect == thin => Some((mask.radius(), mask.stroke())),
            _ => None,
        }).collect();
        assert_eq!(masks, [(12, 2), (12, 2)], "its own size and stroke over the theme's");
    });
    tree.post(tree.root_id, Erased::new(None::<f32>));
    run(&mut tree);
    tree.tick(100);
    run(&mut tree);
    with_ops(&tree, |ops| {
        assert!(ops.iter().any(|op| matches!(op, devkit_gfx::Op::Gradient { from, to, .. } if from.a == 0 && to.a == 255)), "the head leads to the right");
        assert!(ops.iter().any(|op| matches!(op, devkit_gfx::Op::Arc { sweep, from, .. } if *sweep == 270.0 && from.a == 0)));
    });
}

#[test]
fn a_progress_bar_goes_across_and_comes_back() {
    let mut tree = Tree::new(Loading { value: None, shown: true }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let fill = tree.nodes[tree.find("bar").unwrap() as usize].first_child;
    let phase = |tree: &Tree| match &tree.nodes[fill as usize].content {
        NodeContent::Progress(p) => (p.phase, p.backwards),
        _ => unreachable!(),
    };
    tree.tick(500);
    let (t, backwards) = phase(&tree);
    assert!(t > 0.0 && t < 1.0 && !backwards, "{t}");
    tree.tick(1000);
    let (t, backwards) = phase(&tree);
    assert!(t > 0.0 && t < 1.0 && backwards, "{t}");
}

#[test]
fn a_new_screen_size_does_not_ease() {
    let (mut tree, _) = toggled();
    let thumb = tree.find("thumb").unwrap();
    tree.nodes[thumb as usize].rect.x = 10;
    tree.set_area(Rect::new(0, 0, 600, 400));
    run(&mut tree);
    assert!(!tree.animating());
}

fn layout_tree(build: for<'a> fn(&'a Ui<()>, &'static FontChain) -> Element<'a, ()>) -> Tree {
    struct View(for<'a> fn(&'a Ui<()>, &'static FontChain) -> Element<'a, ()>, &'static FontChain);
    impl Component for View {
        type Msg = ();
        fn update(&mut self, _: (), _: &mut Cx<Self>) {}
        fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
            (self.0)(ui, self.1)
        }
    }
    impl Screen for View {}
    let mut tree = Tree::new(View(build, chain()), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    tree
}

const LONG: &str = "AAAA AAAA AAAA AAAA AAAA AAAA AAAA";
const LONGER: &str = "AAAA AAAA AAAA AAAA AAAA AAAA AAAA AAAA AAAA AAAA AAAA AAAA";

#[test]
fn text_in_a_row_shrinks_wraps_and_makes_the_row_taller() {
    let tree = layout_tree(|ui, font| {
        ui.column([
            ui.row([ui.text(font, LONG).wrap().id("a"), ui.text(font, LONG).wrap().id("b")]).id("row"),
            ui.column([]).height(10).id("below"),
        ])
    });
    let (a, b, row) = (rect_of(&tree, "a"), rect_of(&tree, "b"), rect_of(&tree, "row"));
    assert_eq!(a.w + b.w, 400);
    assert_eq!(a.h, 24, "wrapped to two lines of 12: {a:?}");
    assert_eq!(row.h, a.h);
    assert_eq!(rect_of(&tree, "below").y, a.h);
}

#[test]
fn a_fixed_sibling_keeps_its_size_and_a_short_text_does_not_shrink() {
    let tree = layout_tree(|ui, font| {
        ui.column([
            ui.row([ui.column([]).size(100, 10).id("fixed"), ui.text(font, LONGER).wrap().id("long")]).id("first"),
            ui.row([ui.text(font, "AB").id("short"), ui.column([]).height(10).grow(1).id("rest")]),
        ])
    });
    assert_eq!(rect_of(&tree, "fixed").w, 100);
    assert_eq!(rect_of(&tree, "long").w, 300);
    assert!(rect_of(&tree, "long").h > 12);
    assert_eq!(rect_of(&tree, "short").w, 16);
    assert_eq!(rect_of(&tree, "rest").w, 384);
}

#[test]
fn ellipsis_applies_to_the_width_a_row_leaves() {
    let tree = layout_tree(|ui, font| ui.row([ui.text(font, LONG).ellipsis().id("t"), ui.column([]).size(300, 10)]));
    assert_eq!(rect_of(&tree, "t").w, 100);
}

#[test]
fn fractions_and_limits_follow_the_parent() {
    let tree = layout_tree(|ui, font| {
        ui.column([
            ui.column([ui.column([]).width_fraction(1, 4).height(10).id("quarter")]).width(200),
            ui.column([]).height_fraction(1, 2).id("half"),
            ui.text(font, LONG).wrap().max_width(100).id("narrow"),
            ui.column([ui.column([]).height(50)]).max_height(20).id("capped"),
            ui.column([]).height(4).max_width(90).id("stretched"),
        ])
    });
    assert_eq!(rect_of(&tree, "quarter").w, 50);
    assert_eq!(rect_of(&tree, "half").h, 300);
    assert_eq!(rect_of(&tree, "narrow").w, 100);
    assert!(rect_of(&tree, "narrow").h > 12);
    assert_eq!(rect_of(&tree, "capped").h, 20);
    assert_eq!(rect_of(&tree, "stretched").w, 90);
}

#[test]
fn disabled_nodes_take_no_input_and_block_what_is_below() {
    #[derive(Clone, Copy)]
    enum M {
        Tap,
    }
    struct View(&'static core::cell::Cell<u32>, bool);
    impl Component for View {
        type Msg = M;
        fn update(&mut self, _: M, _: &mut Cx<Self>) {
            self.0.set(self.0.get() + 1);
        }
        fn view<'a>(&'a self, ui: &'a Ui<M>) -> Element<'a, M> {
            ui.column([ui.column([]).size(50, 50).id("under").on_tap(M::Tap), ui.column([]).size(50, 50).absolute(Some(0), Some(0), None, None).id("over").on_tap(M::Tap).disabled(self.1)])
        }
    }
    impl Screen for View {}
    let taps = Box::leak(Box::new(core::cell::Cell::new(0)));
    let mut tree = Tree::new(View(taps, true), SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    tree.touch(&[(0, (10, 10))], 0);
    tree.touch(&[], 10);
    tree.update();
    assert_eq!(taps.get(), 0);
    assert!(tree.invoke("over").is_err());
    assert!(tree.describe().contains("disabled"));
}

#[test]
fn an_element_on_the_stack_is_only_a_pointer() {
    assert_eq!(size_of::<Element<'_, ()>>(), size_of::<usize>());
}

#[test]
fn the_heavier_state_wins_in_any_order() {
    const FIXED: fn(Color) -> Option<ColorRole> = |color| Some(ColorRole::Fixed(color));
    static STYLE: StateStyle = StateStyle {
        font: None,
        base: &[],
        states: &[
            (State::PRESSED, &[Prop::Background(ColorRole::Fixed(WHITE))]),
            (State::SELECTED, &[Prop::Background(ColorRole::Fixed(RED)), Prop::Foreground(ColorRole::Fixed(RED))]),
        ],
    };
    let resolved = |state: State, when: &[(State, Prop)]| {
        let mut style = Style::default();
        style.states = Some(&STYLE);
        style.background = FIXED(BLUE);
        style.resolve(state, when);
        style
    };
    assert_eq!(resolved(State::empty(), &[]).background, FIXED(BLUE), "a node's own value stays without a state");
    assert_eq!(resolved(State::SELECTED, &[]).background, FIXED(RED));
    let both = resolved(State::SELECTED | State::PRESSED, &[]);
    assert_eq!((both.background, both.foreground), (FIXED(WHITE), FIXED(RED)));

    let selected = [(State::SELECTED, Prop::Background(ColorRole::Fixed(BLUE)))];
    assert_eq!(resolved(State::SELECTED, &selected).background, FIXED(BLUE), "within a state, when is above the table");
    assert_eq!(resolved(State::SELECTED | State::PRESSED, &selected).background, FIXED(WHITE), "a lighter when leaves a heavier table");
    let combined = [(State::SELECTED | State::PRESSED, Prop::Background(ColorRole::Fixed(BLUE)))];
    assert_eq!(resolved(State::SELECTED | State::PRESSED, &combined).background, FIXED(BLUE), "both states together are heavier");
}

struct Stateful;

impl Screen for Stateful {}

impl Component for Stateful {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut Cx<Self>) {}

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        let pressed_height = ui.screen_size().1 / 10;
        ui.column([
            ui.row([]).height(40).when(State::PRESSED, &[Prop::Height(pressed_height)]).id("grows").on_tap(()),
            ui.row([]).height(10).id("after"),
            widgets::slider(ui, 0, |_| ()).width(200).id("slider").part(Part::Thumb, |thumb| thumb.size(40, 40).id("thumb")),
            ui.row([widgets::slider(ui, 0, |_| ()).width(200).part(Part::Thumb, |thumb| thumb.id("inner-thumb"))]).padding(20).state_source().id("outer").on_tap(()),
        ])
    }
}

#[test]
fn a_state_lays_out_again_and_gives_the_values_back() {
    let mut tree = Tree::new(Stateful, SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    let grows = rect_of(&tree, "grows");
    assert_eq!((grows.h, rect_of(&tree, "after").y), (40, grows.y + 40));
    tree.touch(&[(0, (grows.x + 5, grows.y + 5))], 0);
    tree.update();
    assert_eq!((rect_of(&tree, "grows").h, rect_of(&tree, "after").y), (60, grows.y + 60), "the value comes from a runtime when");
    tree.touch(&[], 16);
    tree.update();
    assert_eq!((rect_of(&tree, "grows").h, rect_of(&tree, "after").y), (40, grows.y + 40));
}

#[test]
fn a_drag_presses_and_parts_take_the_state_of_their_source() {
    let mut tree = Tree::new(Stateful, SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    let pressed = |tree: &Tree, id: &str| tree.nodes[tree.find(id).unwrap() as usize].state.contains(State::PRESSED);
    let slider = rect_of(&tree, "slider");
    tree.touch(&[(0, (slider.x + 100, slider.y + 10))], 0);
    tree.update();
    assert!(pressed(&tree, "slider"));
    assert!(pressed(&tree, "thumb"), "with no rebuild of the part");
    assert!(!pressed(&tree, "inner-thumb"));
    tree.touch(&[], 16);
    tree.update();
    assert!(!pressed(&tree, "slider") && !pressed(&tree, "thumb"));

    let outer = rect_of(&tree, "outer");
    tree.touch(&[(0, (outer.x + 5, outer.y + 5))], 32);
    tree.update();
    assert!(pressed(&tree, "outer"));
    assert!(!pressed(&tree, "inner-thumb"), "a nearer source stops what comes from further out");
    tree.touch(&[], 48);
    tree.update();
}

#[test]
fn a_thumb_of_any_size_stays_centered() {
    let mut tree = Tree::new(Stateful, SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    let thumb = rect_of(&tree, "thumb");
    let slider = rect_of(&tree, "slider");
    assert_eq!((thumb.w, thumb.h), (40, 40));
    assert_eq!(thumb.y + thumb.h / 2, slider.y + slider.h / 2);
}

struct TwoStyles;

impl Screen for TwoStyles {}

impl Component for TwoStyles {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut Cx<Self>) {}

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        static FIRST: StateStyle = StateStyle { font: None, base: &[Prop::Width(10), Prop::Height(10)], states: &[(State::PRESSED, &[Prop::Width(20)])] };
        static SECOND: StateStyle = StateStyle { font: None, base: &[Prop::Height(30)], states: &[] };
        ui.column([ui.row([]).style(&FIRST).style(&SECOND).align_self(Align::Start).id("styled").on_tap(())])
    }
}

#[test]
fn a_second_style_adds_its_base_and_replaces_the_states() {
    let mut tree = Tree::new(TwoStyles, SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    let styled = rect_of(&tree, "styled");
    assert_eq!((styled.w, styled.h), (10, 30));
    tree.touch(&[(0, (styled.x + 5, styled.y + 5))], 0);
    tree.update();
    assert_eq!(rect_of(&tree, "styled").w, 10);
    tree.touch(&[], 16);
    tree.update();
}

#[test]
fn a_text_buffer_edits_around_the_cursor() {
    let mut buffer = TextBuffer::new();
    for c in "aあb".chars() {
        assert!(buffer.apply(TextInput::Char(c)));
    }
    assert_eq!(buffer.as_str(), "aあb");
    assert_eq!(buffer.cursor(), 5);
    buffer.apply(TextInput::Left);
    assert_eq!(buffer.cursor(), 4);
    buffer.apply(TextInput::Backspace);
    assert_eq!(buffer.as_str(), "ab");
    assert_eq!(buffer.cursor(), 1);
    buffer.apply(TextInput::Delete);
    assert_eq!(buffer.as_str(), "a");
    buffer.apply(TextInput::Home);
    assert_eq!(buffer.cursor(), 0);
    assert!(!buffer.apply(TextInput::Backspace));
    buffer.set_text("one\ntwo");
    buffer.apply(TextInput::Up);
    assert_eq!(buffer.cursor(), 3);
    buffer.apply(TextInput::Place(100));
    assert_eq!(buffer.cursor(), 7);
}

#[derive(Clone, Copy)]
enum FormMsg {
    Name(TextInput),
    Pin(TextInput),
    Other,
}

struct Form {
    name: TextBuffer,
    pin: TextBuffer,
}

impl Component for Form {
    type Msg = FormMsg;

    fn update(&mut self, msg: FormMsg, cx: &mut Cx<Self>) {
        match msg {
            FormMsg::Name(input) => {
                self.name.apply(input);
            }
            FormMsg::Pin(input) => {
                self.pin.apply(input);
            }
            FormMsg::Other => {}
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<FormMsg>) -> Element<'a, FormMsg> {
        ui.column([
            widgets::text_field(ui, &self.name, FormMsg::Name).placeholder("Name").id("name"),
            widgets::text_field(ui, &self.pin, FormMsg::Pin).placeholder("PIN").input(InputKind::Number).id("pin"),
            ui.column([]).size(100, 40).id("other").on_tap(FormMsg::Other),
        ])
    }
}

impl Screen for Form {}

fn form_tree() -> Tree {
    let mut tree = Tree::new(Form { name: TextBuffer::new(), pin: TextBuffer::new() }, SCREEN, Caps::INTERNAL, theme());
    tree.add_keyboard(crate::keyboard::KeyboardMode::Auto);
    run(&mut tree);
    tree
}

fn text_of(tree: &Tree, id: &str) -> alloc::string::String {
    let NodeContent::Text(text) = &tree.nodes[tree.find(id).unwrap() as usize].content else { panic!() };
    alloc::string::String::from(text.text())
}

#[test]
fn focus_routes_input_and_shows_the_field_keyboard() {
    let mut tree = form_tree();
    assert!(tree.find("keyboard").is_none());
    assert_eq!(tree.context(0).insets.bottom, 0);

    tap(&mut tree, "name");
    run(&mut tree);
    assert!(tree.find("keyboard").is_none(), "the panel starts below the screen");
    assert_eq!(tree.context(0).insets.bottom, TEXT_KEYS_HEIGHT, "what is below makes room before the panel arrives");
    settle(&mut tree);
    let keyboard = rect_of(&tree, "keyboard");
    assert_eq!(keyboard.bottom(), 600);
    assert_eq!(tree.context(0).insets.bottom, keyboard.h);
    tree.input(TextInput::Char('x'));
    tree.input(TextInput::Char('y'));
    run(&mut tree);
    assert_eq!(text_of(&tree, "name"), "xy");

    assert_eq!(keyboard.h, TEXT_KEYS_HEIGHT);
    tap(&mut tree, "pin");
    run(&mut tree);
    assert_eq!(rect_of(&tree, "keyboard").h, NUMBER_KEYS.height, "each layout has its own height");
    assert_eq!(tree.context(0).insets.bottom, NUMBER_KEYS.height);
    tree.input(TextInput::Char('7'));
    run(&mut tree);
    assert_eq!(text_of(&tree, "pin"), "7");
    assert_eq!(text_of(&tree, "name"), "xy");

    tree.touch(&[(0, (390, 200))], 0);
    tree.touch(&[], 10);
    run(&mut tree);
    assert_eq!(tree.context(0).insets.bottom, 0, "what is below takes the room back as the panel starts to leave");
    assert_eq!(rect_of(&tree, "keyboard").bottom(), 600);
    settle(&mut tree);
    assert!(tree.find("keyboard").is_none(), "a tap elsewhere ends the input");
}

#[test]
fn a_turned_screen_caps_the_keyboard_height() {
    let mut tree = form_tree();
    tap(&mut tree, "pin");
    settle(&mut tree);
    assert_eq!(rect_of(&tree, "keyboard").h, 200);

    tree.set_area(Rect::new(0, 0, 600, 400));
    run(&mut tree);
    let keyboard = rect_of(&tree, "keyboard");
    assert_eq!((keyboard.h, keyboard.bottom()), (160, 400));
    assert_eq!(tree.context(0).insets.bottom, 160);
}

#[test]
fn a_leaving_keyboard_keeps_its_place_through_a_layout() {
    let mut tree = form_tree();
    tap(&mut tree, "name");
    settle(&mut tree);
    let shown = rect_of(&tree, "keyboard");
    tree.touch(&[(0, (390, 200))], 0);
    tree.touch(&[], 10);
    run(&mut tree);
    tree.layout_dirty = true;
    tree.tick(16);
    run(&mut tree);
    let leaving = tree.nodes.iter().find(|n| n.id == Some("keyboard")).unwrap();
    assert!(leaving.leaving);
    assert_eq!(leaving.rect, shown);
}

#[test]
fn a_physical_keyboard_hides_the_panel_and_keeps_input_working() {
    let mut tree = form_tree();
    tree.set_keyboard_connected(true);
    tap(&mut tree, "name");
    settle(&mut tree);
    assert!(tree.find("keyboard").is_none());
    tree.input(TextInput::Char('k'));
    run(&mut tree);
    assert_eq!(text_of(&tree, "name"), "k");
    tree.set_keyboard_connected(false);
    settle(&mut tree);
    assert!(tree.find("keyboard").is_some());
}

#[test]
fn keys_line_up_across_rows_and_type_through_layers() {
    let mut tree = form_tree();
    tap(&mut tree, "name");
    settle(&mut tree);
    let keyboard = rect_of(&tree, "keyboard");
    let press = |tree: &mut Tree, x: i16, y: i16| {
        tree.touch(&[(0, (x, y))], 0);
        tree.touch(&[], 10);
        run(tree);
    };
    let k = theme().widgets.keyboard;
    let pitch = (400 - k.gap) / 10;
    let row_y = |row: i16| keyboard.y + k.gap + row * KEY_PITCH + 10;
    // Shift, then one upper case letter, then back to lower case.
    press(&mut tree, k.gap + 10, row_y(2));
    press(&mut tree, k.gap + pitch + 10, row_y(0));
    press(&mut tree, k.gap + pitch + 10, row_y(0));
    assert_eq!(text_of(&tree, "name"), "Ww");
    // A double tap locks upper case, a third tap leaves it.
    press(&mut tree, k.gap + 10, row_y(2));
    press(&mut tree, k.gap + 10, row_y(2));
    press(&mut tree, k.gap + pitch + 10, row_y(0));
    press(&mut tree, k.gap + pitch + 10, row_y(0));
    press(&mut tree, k.gap + 10, row_y(2));
    press(&mut tree, k.gap + pitch + 10, row_y(0));
    assert_eq!(text_of(&tree, "name"), "WwWWw");
    // A tap between keys neither ends the input nor reaches what is beneath.
    press(&mut tree, 2, row_y(0));
    assert_eq!(text_of(&tree, "name"), "WwWWw");
    assert!(tree.find("keyboard").is_some());
    // Symbols stay until asked to leave.
    press(&mut tree, k.gap + 10, row_y(3));
    press(&mut tree, k.gap + 10, row_y(0));
    press(&mut tree, k.gap + 10, row_y(0));
    assert_eq!(text_of(&tree, "name"), "WwWWw11");
}

#[test]
fn a_long_field_keeps_its_width_and_the_caret_in_view() {
    let mut tree = form_tree();
    let empty = rect_of(&tree, "name");
    tap(&mut tree, "name");
    settle(&mut tree);
    for _ in 0..200 {
        tree.input(TextInput::Char('A'));
    }
    run(&mut tree);
    let field = rect_of(&tree, "name");
    assert_eq!((field.w, field.h), (empty.w, empty.h));
    let NodeContent::Text(text) = &tree.nodes[tree.find("name").unwrap() as usize].content else { panic!() };
    let [left, _, right, _] = tree.nodes[tree.find("name").unwrap() as usize].style.padding;
    let width = field.w - left - right;
    assert_eq!(text.scroll(width), text.run.caret(200).unwrap().0 + 2 - width);
}

#[test]
fn a_field_scrolls_only_when_the_cursor_leaves_it() {
    let mut tree = form_tree();
    tap(&mut tree, "name");
    settle(&mut tree);
    for _ in 0..200 {
        tree.input(TextInput::Char('A'));
    }
    run(&mut tree);
    let id = tree.find("name").unwrap() as usize;
    let [left, _, right, _] = tree.nodes[id].style.padding;
    let width = tree.nodes[id].screen.w - left - right;
    let scroll = |tree: &Tree| {
        let NodeContent::Text(text) = &tree.nodes[id].content else { panic!() };
        text.scroll(width)
    };
    let end = scroll(&tree);
    tree.input(TextInput::Left);
    run(&mut tree);
    assert_eq!(scroll(&tree), end);
    tree.input(TextInput::Home);
    run(&mut tree);
    assert_eq!(scroll(&tree), 0);
    tree.input(TextInput::End);
    run(&mut tree);
    assert_eq!(scroll(&tree), end);
}

#[test]
fn a_tap_in_a_field_places_the_cursor() {
    let mut tree = form_tree();
    tap(&mut tree, "name");
    settle(&mut tree);
    tree.input(TextInput::Char('A'));
    tree.input(TextInput::Char('B'));
    run(&mut tree);
    let field = rect_of(&tree, "name");
    let left = tree.nodes[tree.find("name").unwrap() as usize].style.padding[0];
    tree.touch(&[(0, (field.x + left + 11, field.y + 10))], 0);
    tree.touch(&[], 10);
    run(&mut tree);
    tree.input(TextInput::Char('x'));
    run(&mut tree);
    assert_eq!(text_of(&tree, "name"), "AxB");
}

struct TopChrome {
    height: i16,
}

impl Component for TopChrome {
    type Msg = i16;

    fn update(&mut self, height: i16, cx: &mut Cx<Self>) {
        self.height = height;
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<i16>) -> Element<'a, i16> {
        if self.height == 0 { ui.empty() } else { ui.column([]).height(self.height).id("chrome") }
    }
}

struct Padded;

impl Screen for Padded {}

impl Component for Padded {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut Cx<Self>) {}

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        ui.column([ui.column([]).height(10).id("first")]).padding_each(0, ui.insets().top, 0, 0)
    }
}

#[test]
fn the_chrome_is_as_tall_as_it_comes_out_and_screens_start_below_it() {
    let mut tree = Tree::new(Padded, SCREEN, Caps::INTERNAL, theme());
    let chrome = tree.add_chrome(TopChrome { height: 30 });
    run(&mut tree);
    assert_eq!(tree.context(0).insets.top, 30);
    assert_eq!(rect_of(&tree, "first").y, 30);
    let id = tree.host_id(chrome);
    tree.post(id, Erased::new(50i16));
    run(&mut tree);
    assert_eq!(rect_of(&tree, "first").y, 50, "built again with the new height in the same update");
    tree.post(id, Erased::new(0i16));
    run(&mut tree);
    assert_eq!(rect_of(&tree, "first").y, 0, "a chrome showing nothing covers nothing");
}

#[test]
fn a_layer_keeps_the_panel_height_and_shares_it_between_its_rows() {
    let mut tree = form_tree();
    tap(&mut tree, "name");
    settle(&mut tree);
    let keyboard = rect_of(&tree, "keyboard");
    let gap = theme().widgets.keyboard.gap;
    let press = |tree: &mut Tree, x: i16, y: i16| {
        tree.touch(&[(0, (x, y))], 0);
        tree.touch(&[], 10);
        run(tree);
    };
    press(&mut tree, gap + 10, keyboard.y + gap + 3 * KEY_PITCH + 10);
    assert_eq!(rect_of(&tree, "keyboard"), keyboard);
    assert_eq!(tree.context(0).insets.bottom, keyboard.h);
    press(&mut tree, keyboard.right() / 4 + 10, keyboard.bottom() - gap - 2);
    press(&mut tree, keyboard.right() / 4 + 10, keyboard.y + gap + 2);
    assert_eq!(text_of(&tree, "name"), "32", "the columns and the taller rows are those of this layer");
}

#[test]
fn full_rows_end_at_the_panel_edge() {
    let mut tree = form_tree();
    tap(&mut tree, "name");
    settle(&mut tree);
    let keyboard = rect_of(&tree, "keyboard");
    let gap = theme().widgets.keyboard.gap;
    let y = keyboard.y + gap + 2 * KEY_PITCH + 10;
    tree.touch(&[(0, (keyboard.right() - gap - 1, y))], 0);
    tree.update();
    assert!(tree.timed(), "the last pixel of the row is the repeating key");
}

#[test]
fn a_held_key_repeats_and_its_release_adds_nothing() {
    let mut tree = form_tree();
    tap(&mut tree, "name");
    settle(&mut tree);
    for _ in 0..3 {
        tree.input(TextInput::Char('a'));
    }
    run(&mut tree);
    assert_eq!(text_of(&tree, "name"), "aaa");
    let keyboard = rect_of(&tree, "keyboard");
    let k = theme().widgets.keyboard;
    let y = keyboard.y + k.gap + 2 * KEY_PITCH + 10;
    tree.touch(&[(0, (380, y))], 0);
    tree.update();
    assert!(tree.timed());
    tree.tick(400);
    tree.update();
    tree.tick(60);
    run(&mut tree);
    assert_eq!(text_of(&tree, "name"), "a");
    tree.touch(&[], 500);
    run(&mut tree);
    assert_eq!(text_of(&tree, "name"), "a");
}

struct Pair;

impl Component for Pair {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut Cx<Self>) {}

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        ui.row([ui.list(0, 100, 40).grow(1).id("left"), ui.list(1, 100, 40).grow(1).id("right")])
    }

    fn list_row<'a>(&'a self, _: ListId, _: usize, ui: &'a Ui<()>) -> Element<'a, ()> {
        ui.column([]).height(40)
    }
}

impl Screen for Pair {}

fn offset_of(tree: &Tree, id: &str) -> i32 {
    let NodeContent::List(list) = &tree.nodes[tree.find(id).unwrap() as usize].content else { panic!() };
    list.offset
}

#[test]
fn two_fingers_scroll_two_lists_apart_and_throw_both() {
    let mut tree = Tree::new(Pair, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let (left, right) = (rect_of(&tree, "left"), rect_of(&tree, "right"));
    let (a, b) = (left.x + 10, right.x + 10);
    tree.touch(&[(3, (a, 400)), (7, (b, 400))], 0);
    tree.touch(&[(3, (a, 340)), (7, (b, 400))], 10);
    tree.touch(&[(3, (a, 280)), (7, (b, 370))], 20);
    tree.touch(&[(3, (a, 220)), (7, (b, 340))], 30);
    assert_eq!(offset_of(&tree, "left"), 180);
    assert_eq!(offset_of(&tree, "right"), 60);
    tree.touch(&[(7, (b, 310))], 40);
    assert!(tree.animating(), "one list thrown while the other is held");
    tree.touch(&[], 45);
    let before = (offset_of(&tree, "left"), offset_of(&tree, "right"));
    tree.tick(16);
    assert!(offset_of(&tree, "left") > before.0);
    assert!(offset_of(&tree, "right") > before.1);
}

#[test]
fn a_press_stops_only_the_fling_it_lands_on() {
    let mut tree = Tree::new(Pair, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let (left, right) = (rect_of(&tree, "left"), rect_of(&tree, "right"));
    let (a, b) = (left.x + 10, right.x + 10);
    for (i, y) in [400, 340, 280, 220].into_iter().enumerate() {
        tree.touch(&[(0, (a, y))], i as u32 * 10);
    }
    tree.touch(&[], 35);
    tree.touch(&[(1, (b, 400))], 40);
    let before = offset_of(&tree, "left");
    tree.tick(16);
    assert!(offset_of(&tree, "left") > before);
    tree.touch(&[(1, (b, 400)), (2, (a, 400))], 50);
    let before = offset_of(&tree, "left");
    tree.tick(16);
    assert_eq!(offset_of(&tree, "left"), before);
}

#[test]
fn a_second_finger_on_a_held_list_does_nothing() {
    let mut tree = Tree::new(Pair, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let left = rect_of(&tree, "left");
    let a = left.x + 10;
    tree.touch(&[(0, (a, 400))], 0);
    tree.touch(&[(0, (a, 340)), (1, (a + 20, 400))], 10);
    tree.touch(&[(0, (a, 340)), (1, (a + 20, 200))], 20);
    assert_eq!(offset_of(&tree, "left"), 60);
    tree.touch(&[(1, (a + 20, 100))], 200);
    tree.touch(&[(1, (a + 20, 50))], 210);
    assert_eq!(offset_of(&tree, "left"), 60, "the first finger left, the second still does nothing");
    tree.touch(&[], 220);
    assert!(!tree.animating());
}

#[test]
fn keys_pressed_together_both_type_in_release_order() {
    let mut tree = form_tree();
    tap(&mut tree, "name");
    settle(&mut tree);
    let keyboard = rect_of(&tree, "keyboard");
    let k = theme().widgets.keyboard;
    let pitch = (keyboard.w - k.gap) / 10;
    let y = keyboard.y + k.gap + 10;
    let (q, w) = (k.gap + 10, k.gap + pitch + 10);
    tree.touch(&[(0, (q, y))], 0);
    tree.touch(&[(0, (q, y)), (1, (w, y))], 10);
    tree.touch(&[(0, (q, y))], 20);
    tree.touch(&[], 30);
    run(&mut tree);
    assert_eq!(text_of(&tree, "name"), "wq");
}

#[test]
fn a_finger_on_a_held_key_does_not_end_the_input() {
    let mut tree = form_tree();
    tap(&mut tree, "name");
    settle(&mut tree);
    let keyboard = rect_of(&tree, "keyboard");
    let y = keyboard.y + theme().widgets.keyboard.gap + 10;
    let x = theme().widgets.keyboard.gap + 10;
    tree.touch(&[(0, (x, y))], 0);
    tree.touch(&[(0, (x, y)), (1, (x + 2, y))], 10);
    tree.touch(&[(0, (x, y))], 20);
    run(&mut tree);
    assert!(tree.find("keyboard").is_some());
    tree.touch(&[], 30);
    run(&mut tree);
    assert_eq!(text_of(&tree, "name"), "q");
}

#[derive(Clone, Copy)]
enum PadMsg {
    Gesture(Gesture),
    Tap,
}

struct Pad {
    gestures: Gestures,
    log: alloc::string::String,
}

impl Component for Pad {
    type Msg = PadMsg;

    fn update(&mut self, msg: PadMsg, cx: &mut Cx<Self>) {
        use core::fmt::Write;
        let log = &mut self.log;
        let _ = match msg {
            PadMsg::Tap => write!(log, "tap "),
            PadMsg::Gesture(Gesture::Down { .. }) => write!(log, "down "),
            PadMsg::Gesture(Gesture::Up { .. }) => write!(log, "up "),
            PadMsg::Gesture(Gesture::Pan { phase, dx, dy, .. }) => write!(log, "pan:{phase:?}:{dx},{dy} "),
            PadMsg::Gesture(Gesture::Pinch { phase, scale, .. }) => write!(log, "pinch:{phase:?}:{scale:.1} "),
            PadMsg::Gesture(Gesture::Rotate { phase, angle, .. }) => write!(log, "rotate:{phase:?}:{angle:.1} "),
            PadMsg::Gesture(Gesture::DoubleTap { .. }) => write!(log, "double "),
            PadMsg::Gesture(Gesture::LongPress { .. }) => write!(log, "long "),
            PadMsg::Gesture(Gesture::Swipe { direction }) => write!(log, "swipe:{direction:?} "),
        };
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<PadMsg>) -> Element<'a, PadMsg> {
        ui.column([
            ui.text(chain(), &self.log).height(20).id("log"),
            ui.column([]).size(300, 300).id("pad").gestures(self.gestures, PadMsg::Gesture).on_tap(PadMsg::Tap),
            ui.column([]).size(300, 100).id("elsewhere"),
        ])
    }
}

impl Screen for Pad {}

fn pad(gestures: Gestures) -> Tree {
    let mut tree = Tree::new(Pad { gestures, log: alloc::string::String::new() }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    tree
}

fn log_of(tree: &mut Tree) -> alloc::string::String {
    run(tree);
    let mut log = text_of(tree, "log");
    log.truncate(log.trim_end().len());
    log
}

#[test]
fn press_comes_around_an_immediate_tap() {
    let mut tree = pad(Gestures::PRESS);
    let r = rect_of(&tree, "pad");
    tree.touch(&[(0, (r.x + 50, r.y + 50))], 0);
    assert_eq!(log_of(&mut tree), "down");
    tree.touch(&[], 50);
    assert_eq!(log_of(&mut tree), "down up tap");
}

#[test]
fn a_tap_waits_for_a_double_tap_while_press_does_not() {
    let mut tree = pad(Gestures::PRESS | Gestures::DOUBLE_TAP);
    let r = rect_of(&tree, "pad");
    let p = (r.x + 50, r.y + 50);
    tree.touch(&[(0, p)], 0);
    tree.touch(&[], 50);
    assert_eq!(log_of(&mut tree), "down up");
    assert!(tree.timed());
    tree.tick(299);
    assert_eq!(log_of(&mut tree), "down up");
    tree.tick(1);
    assert_eq!(log_of(&mut tree), "down up tap");
    assert!(tree.idle());

    tree.touch(&[(0, p)], 1000);
    tree.touch(&[], 1050);
    tree.tick(100);
    tree.touch(&[(0, p)], 1150);
    tree.touch(&[], 1200);
    assert_eq!(log_of(&mut tree), "down up tap down up down up double");
    tree.tick(1000);
    assert_eq!(log_of(&mut tree), "down up tap down up down up double");
}

#[test]
fn a_far_second_tap_is_two_taps() {
    let mut tree = pad(Gestures::DOUBLE_TAP);
    let r = rect_of(&tree, "pad");
    tree.touch(&[(0, (r.x + 10, r.y + 10))], 0);
    tree.touch(&[], 50);
    tree.touch(&[(0, (r.x + 200, r.y + 200))], 100);
    tree.touch(&[], 150);
    tree.tick(300);
    assert_eq!(log_of(&mut tree), "tap tap");
}

#[test]
fn a_pan_replaces_the_tap_and_reports_once_per_frame() {
    let mut tree = pad(Gestures::PAN);
    let r = rect_of(&tree, "pad");
    let (x, y) = (r.x + 50, r.y + 50);
    tree.touch(&[(0, (x, y))], 0);
    tree.touch(&[(0, (x + 30, y))], 10);
    tree.touch(&[(0, (x + 40, y + 5))], 20);
    tree.touch(&[(0, (x + 50, y + 10))], 30);
    assert_eq!(log_of(&mut tree), "pan:Began:30,0 pan:Changed:50,10");
    tree.touch(&[], 40);
    assert_eq!(log_of(&mut tree), "pan:Began:30,0 pan:Changed:50,10 pan:Ended:50,10");
}

#[test]
fn two_fingers_on_a_node_pinch_and_rotate_until_one_lifts() {
    let mut tree = pad(Gestures::PINCH | Gestures::ROTATE | Gestures::PAN);
    let r = rect_of(&tree, "pad");
    let (a, b) = ((r.x + 100, r.y + 100), (r.x + 200, r.y + 100));
    tree.touch(&[(0, a)], 0);
    tree.touch(&[(0, a), (1, b)], 10);
    assert_eq!(log_of(&mut tree), "pinch:Began:1.0 rotate:Began:0.0");
    tree.touch(&[(0, a), (1, (a.0, a.1 + 200))], 20);
    assert_eq!(log_of(&mut tree), "pinch:Began:1.0 rotate:Began:0.0 pinch:Changed:2.0 rotate:Changed:1.6");
    tree.touch(&[(1, (a.0, a.1 + 200))], 30);
    tree.touch(&[(1, (a.0 + 80, a.1 + 200))], 40);
    tree.touch(&[], 50);
    let log = log_of(&mut tree);
    assert!(log.ends_with("pinch:Ended:2.0 rotate:Ended:1.6"), "the finger left behind does not pan: {log}");
}

#[test]
fn a_second_finger_outside_the_node_is_no_pinch() {
    let mut tree = pad(Gestures::PINCH);
    let r = rect_of(&tree, "pad");
    let other = rect_of(&tree, "elsewhere");
    tree.touch(&[(0, (r.x + 10, r.y + 10))], 0);
    tree.touch(&[(0, (r.x + 10, r.y + 10)), (1, (other.x + 10, other.y + 10))], 10);
    tree.touch(&[], 20);
    assert_eq!(log_of(&mut tree), "tap");
}

#[test]
fn a_long_press_takes_the_place_of_the_tap() {
    let mut tree = pad(Gestures::LONG_PRESS);
    let r = rect_of(&tree, "pad");
    tree.touch(&[(0, (r.x + 10, r.y + 10))], 0);
    assert!(tree.timed());
    tree.tick(500);
    assert_eq!(log_of(&mut tree), "long");
    tree.touch(&[], 600);
    assert_eq!(log_of(&mut tree), "long");
}

#[test]
fn a_quick_stroke_is_a_swipe_and_a_slow_one_is_not() {
    let mut tree = pad(Gestures::SWIPE);
    let r = rect_of(&tree, "pad");
    let (x, y) = (r.x + 200, r.y + 100);
    tree.touch(&[(0, (x, y))], 0);
    tree.touch(&[(0, (x - 100, y + 10))], 100);
    tree.touch(&[], 150);
    assert_eq!(log_of(&mut tree), "swipe:Left");
    tree.touch(&[(0, (x, y))], 1000);
    tree.touch(&[(0, (x - 100, y))], 1200);
    tree.touch(&[], 1500);
    assert_eq!(log_of(&mut tree), "swipe:Left");
}

struct Photos {
    pinches: u32,
}

impl Component for Photos {
    type Msg = Gesture;

    fn update(&mut self, msg: Gesture, _: &mut Cx<Self>) {
        if matches!(msg, Gesture::Pinch { .. }) {
            self.pinches += 1;
            PINCHES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<Gesture>) -> Element<'a, Gesture> {
        ui.list(0, 20, 300).grow(1).id("photos")
    }

    fn list_row<'a>(&'a self, _: ListId, _: usize, ui: &'a Ui<Gesture>) -> Element<'a, Gesture> {
        ui.column([]).height(300).gestures(Gestures::PINCH, |g| g)
    }
}

impl Screen for Photos {}

static PINCHES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[test]
fn one_finger_scrolls_past_a_pinch_and_a_second_stops_it() {
    let mut tree = Tree::new(Photos { pinches: 0 }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let r = rect_of(&tree, "photos");
    let x = r.x + 50;
    tree.touch(&[(0, (x, r.y + 200))], 0);
    tree.touch(&[(0, (x, r.y + 140))], 10);
    tree.touch(&[(0, (x, r.y + 80))], 20);
    assert_eq!(offset_of(&tree, "photos"), 120);
    run(&mut tree);
    assert_eq!(PINCHES.load(core::sync::atomic::Ordering::Relaxed), 0);
    tree.touch(&[(0, (x, r.y + 80)), (1, (x + 40, r.y + 80))], 30);
    tree.touch(&[(0, (x, r.y + 40)), (1, (x + 40, r.y + 80))], 40);
    run(&mut tree);
    assert_eq!(offset_of(&tree, "photos"), 120);
    assert!(PINCHES.load(core::sync::atomic::Ordering::Relaxed) >= 1);
    tree.touch(&[], 50);
    assert!(!tree.animating());
}

static VIEWS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[derive(Clone, Copy)]
enum ScopeMsg {
    Sized(i16, i16),
    Toggle,
    Part,
}

struct Scope {
    tall: bool,
    size: (i16, i16),
}

struct Ticks;

impl devkit_gfx::Raster for Ticks {
    fn raster(&self, _: &mut devkit_gfx::RasterTarget<'_>) {}
}

static TICKS: Ticks = Ticks;

impl Component for Scope {
    type Msg = ScopeMsg;

    fn update(&mut self, msg: ScopeMsg, cx: &mut Cx<Self>) {
        match msg {
            ScopeMsg::Sized(width, height) => self.size = (width, height),
            ScopeMsg::Toggle => {
                self.tall = !self.tall;
                cx.rebuild();
            }
            ScopeMsg::Part => cx.redraw_rect("scope", Rect::new(10, 20, 30, 4)),
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<ScopeMsg>) -> Element<'a, ScopeMsg> {
        VIEWS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        ui.column([
            ui.column([]).height(10),
            ui.canvas(3).height(if self.tall { 200 } else { 100 }).padding(5).id("scope").on_size(ScopeMsg::Sized),
            ui.column([]).size(10, 10).id("toggle").on_tap(ScopeMsg::Toggle),
            ui.column([]).size(10, 10).id("part").on_tap(ScopeMsg::Part),
        ])
    }

    fn draw<'a>(&'a self, id: CanvasId, canvas: &mut Canvas<'a, '_>) {
        assert_eq!(id, 3);
        assert_eq!(canvas.size(), self.size, "the size arrives before the drawing at it");
        assert!(canvas.needs(Rect::new(0, 0, 4, 4)));
        assert!(!canvas.needs(Rect::new(0, 80, 4, 4)));
        canvas.fill(Rect::new(1, 2, 3, 4), Color::rgb(1, 2, 3));
        canvas.push_clip(Rect::new(0, 0, 5, 5));
        canvas.raster(Rect::new(0, 0, 5, 5), &TICKS);
    }
}

impl Screen for Scope {}

fn paint_scope(tree: &Tree, regions: &[Rect]) -> Vec<devkit_gfx::Rect> {
    let mut effects = crate::paint::Effects::new(Caps::INTERNAL);
    let mut list = devkit_gfx::DisplayList::new_in(Caps::INTERNAL);
    tree.paint(&mut effects, regions, &mut list);
    let mut fills = Vec::new();
    let (mut pushed, mut popped) = (0, 0);
    for op in list.ops() {
        match op {
            devkit_gfx::Op::Fill { rect, .. } => fills.push(*rect),
            devkit_gfx::Op::PushClip(_) => pushed += 1,
            devkit_gfx::Op::PopClip => popped += 1,
            _ => {}
        }
    }
    assert_eq!(pushed, popped, "clips left open are closed");
    fills
}

#[test]
fn a_canvas_learns_its_size_before_drawing_and_again_when_it_changes() {
    let mut tree = Tree::new(Scope { tall: false, size: (0, 0) }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let r = rect_of(&tree, "scope");
    assert_eq!(r.h, 100);
    let regions = [Rect::new(r.x, r.y, r.w, 20)];
    assert_eq!(paint_scope(&tree, &regions), [Rect::new(r.x + 6, r.y + 7, 3, 4)]);
    tap(&mut tree, "toggle");
    run(&mut tree);
    assert_eq!(rect_of(&tree, "scope").h, 200);
    paint_scope(&tree, &regions);
}

#[test]
fn redrawing_a_part_damages_it_alone_without_a_view() {
    let mut tree = Tree::new(Scope { tall: false, size: (0, 0) }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let r = rect_of(&tree, "scope");
    tree.take_damage();
    let views = VIEWS.load(core::sync::atomic::Ordering::Relaxed);
    let point = tree.tap_point("part").unwrap();
    tree.touch(&[(0, point)], 0);
    tree.touch(&[], 10);
    tree.update();
    let damage = tree.take_damage();
    assert!(damage.contains(&Rect::new(r.x + 15, r.y + 25, 30, 4)), "{damage:?}");
    assert!(damage.iter().all(|d| d.h <= 10), "{damage:?}");
    assert_eq!(VIEWS.load(core::sync::atomic::Ordering::Relaxed), views);
}

struct Player {
    video: ExternalSurface,
    menu: bool,
}

impl Component for Player {
    type Msg = ();

    fn update(&mut self, _: (), cx: &mut Cx<Self>) {
        self.menu = true;
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        let menu = if self.menu { ui.column([]).size(50, 20).background(Color::rgb(1, 2, 3)).absolute(Some(10), Some(40), None, None).id("menu") } else { ui.empty() };
        ui.column([
            ui.column([]).height(23).id("open").on_tap(()),
            ui.external(&self.video).height(100).id("video"),
            menu,
        ])
    }
}

impl Screen for Player {}

fn overlap_of(tree: &Tree, hole: Rect) -> Option<Rect> {
    let mut effects = crate::paint::Effects::new(Caps::INTERNAL);
    let mut list = devkit_gfx::DisplayList::new_in(Caps::INTERNAL);
    tree.paint(&mut effects, &[SCREEN], &mut list);
    devkit_gfx::overlap(list.ops(), tree.external_split.get(), &hole)
}

#[test]
fn a_surface_keeps_to_the_row_grid_and_sees_what_lies_over_it() {
    let video = ExternalSurface::new().unwrap();
    let mut tree = Tree::new(Player { video, menu: false }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let (_, hole) = tree.external().unwrap();
    let hole = hole.unwrap();
    assert_eq!((hole.y, hole.bottom()), (32, 112));
    assert_eq!(overlap_of(&tree, hole), None);
    tap(&mut tree, "open");
    run(&mut tree);
    assert_eq!(overlap_of(&tree, hole), Some(Rect::new(10, 40, 50, 20)));
}

#[test]
fn portrait_and_landscape_follow_the_shape_of_the_panel() {
    let tall = (720, 1280);
    let wide = (1280, 720);
    let (r0, r90, r180, r270) = (Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270);
    assert_eq!(Rotations::LANDSCAPE.pick(r90, r0, tall), r90);
    assert_eq!(Rotations::LANDSCAPE.pick(r0, r0, tall), r90, "the first turn allowed");
    assert_eq!(Rotations::LANDSCAPE.pick(r180, r270, tall), r270, "the current one is still fine");
    assert_eq!(Rotations::LANDSCAPE.pick(r0, r180, wide), r0);
    assert_eq!(Rotations::PORTRAIT.pick(r0, r0, wide), r90);
    assert_eq!(Rotations::CURRENT.pick(r90, r0, tall), r0);
    assert_eq!(Rotations::default().pick(r270, r0, tall), r270);
    assert_eq!((Rotations::R0 | Rotations::R180).pick(r90, r180, tall), r180);
}

#[test]
fn turning_lays_out_again_and_drops_the_fingers() {
    let mut tree = Tree::new(Scope { tall: false, size: (0, 0) }, Rect::new(0, 0, 400, 600), Caps::INTERNAL, theme());
    run(&mut tree);
    assert_eq!(rect_of(&tree, "scope").w, 400);
    let point = tree.tap_point("toggle").unwrap();
    tree.touch(&[(0, point)], 0);
    tree.set_area(Rect::new(0, 0, 600, 400));
    run(&mut tree);
    assert_eq!(rect_of(&tree, "scope").w, 600);
    tree.touch(&[], 10);
    run(&mut tree);
    assert_eq!(rect_of(&tree, "scope").h, 100, "the finger's tap is gone");
}

static HOME_POSTS: AtomicU32 = AtomicU32::new(0);
static LEAF_POSTS: AtomicU32 = AtomicU32::new(0);
static LEAF_ID: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
enum HomeMsg {
    Open,
    Posted,
    Close,
}

struct Home;

impl Component for Home {
    type Msg = HomeMsg;

    fn update(&mut self, msg: HomeMsg, cx: &mut Cx<Self>) {
        match msg {
            HomeMsg::Open => cx.nav().push(Leaf),
            HomeMsg::Posted => {
                HOME_POSTS.fetch_add(1, Relaxed);
            }
            HomeMsg::Close => cx.nav().root(),
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<HomeMsg>) -> Element<'a, HomeMsg> {
        ui.column([]).size(100, 40).id("home").on_tap(HomeMsg::Open)
    }
}

impl Screen for Home {}

#[derive(Clone, Copy)]
enum LeafMsg {
    Open,
    Posted,
    Back,
}

struct Leaf;

impl Component for Leaf {
    type Msg = LeafMsg;

    fn update(&mut self, msg: LeafMsg, cx: &mut Cx<Self>) {
        match msg {
            LeafMsg::Open => cx.nav().push(Leaf),
            LeafMsg::Back => cx.nav().pop(),
            LeafMsg::Posted => {
                LEAF_POSTS.fetch_add(1, Relaxed);
            }
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<LeafMsg>) -> Element<'a, LeafMsg> {
        ui.column([]).size(100, 40).id("leaf").on_tap(LeafMsg::Open)
    }
}

impl Screen for Leaf {
    fn on_enter(&mut self, cx: &mut Cx<Self>) {
        LEAF_ID.store(cx.handle().id(), Relaxed);
    }
}

#[test]
fn a_handle_follows_its_component_and_nothing_else() {
    let mut tree = Tree::new(Home, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let home = tree.root_id;
    let posted = Erased::new(HomeMsg::Posted);

    tree.post(home, posted);
    assert_eq!(HOME_POSTS.load(Relaxed), 1);

    tap(&mut tree, "home");
    settle(&mut tree);
    let first_leaf = LEAF_ID.load(Relaxed);
    tree.post(home, posted);
    assert_eq!(HOME_POSTS.load(Relaxed), 2, "reaches a covered screen");

    tap(&mut tree, "leaf");
    settle(&mut tree);
    tree.post(home, posted);
    assert_eq!(HOME_POSTS.load(Relaxed), 3, "and not whoever took its slot");
    assert_eq!(LEAF_POSTS.load(Relaxed), 0);

    tree.post(first_leaf, Erased::new(LeafMsg::Posted));
    assert_eq!(LEAF_POSTS.load(Relaxed), 1);

    tree.post(home, Erased::new(HomeMsg::Close));
    settle(&mut tree);
    assert!(tree.find("home").is_some(), "a covered screen can move its navigator");
    tree.post(first_leaf, Erased::new(LeafMsg::Posted));
    assert_eq!(LEAF_POSTS.load(Relaxed), 1, "a closed screen gets nothing");
}

struct Panes;

#[derive(Clone, Copy)]
enum SplitMsg {
    Open,
    Back,
    Deeper,
    Cover,
}

struct Listing;

impl Component for Listing {
    type Msg = SplitMsg;

    fn update(&mut self, _: SplitMsg, cx: &mut Cx<Self>) {
        cx.nav().show(Sheet(0));
    }

    fn view<'a>(&'a self, ui: &'a Ui<SplitMsg>) -> Element<'a, SplitMsg> {
        let role = match ui.split_role() {
            SplitRole::None => "none",
            SplitRole::Main { detail: false } => "main",
            SplitRole::Main { detail: true } => "main-open",
            SplitRole::Detail => "detail",
        };
        ui.text(chain(), role).id("listing").on_tap(SplitMsg::Open)
    }
}

impl Screen for Listing {}

impl Child for Listing {
    fn create(_: &()) -> Self {
        Self
    }

    fn set_props(&mut self, _: &()) {}
}

struct Sheet(u8);

impl Component for Sheet {
    type Msg = SplitMsg;

    fn update(&mut self, msg: SplitMsg, cx: &mut Cx<Self>) {
        match msg {
            SplitMsg::Back => cx.nav().pop(),
            SplitMsg::Deeper => cx.nav().push(Sheet(self.0 + 1)),
            SplitMsg::Cover => cx.nav_of::<Root>().push(Leaf),
            SplitMsg::Open => {}
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<SplitMsg>) -> Element<'a, SplitMsg> {
        let back = if ui.can_pop() { "back" } else { "stay" };
        ui.column([
            ui.text(chain(), back).id(["depth0", "depth1"][usize::from(self.0)]),
            ui.column([]).size(50, 40).id(["deeper0", "deeper1"][usize::from(self.0)]).on_tap(SplitMsg::Deeper),
            ui.column([]).size(50, 40).id(["back0", "back1"][usize::from(self.0)]).on_tap(SplitMsg::Back),
            ui.column([]).size(50, 40).id("cover0").on_tap(SplitMsg::Cover),
        ])
    }
}

impl Screen for Sheet {}

struct SplitHost {
    wide: bool,
    default: bool,
}

impl Component for SplitHost {
    type Msg = bool;

    fn update(&mut self, wide: bool, cx: &mut Cx<Self>) {
        self.wide = wide;
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<bool>) -> Element<'a, bool> {
        let nav = ui.navigator::<Panes, Listing>(()).split(self.wide, 100).grow(1);
        ui.column([if self.default { nav.split_detail(|| Sheet(0)) } else { nav }])
    }
}

impl Screen for SplitHost {}

fn split_tree(default: bool) -> Tree {
    let mut tree = Tree::new(SplitHost { wide: true, default }, SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    tree
}

fn turn(tree: &mut Tree, wide: bool) {
    tree.post(tree.root_id, Erased::new(wide));
    settle(tree);
}

fn go(tree: &mut Tree, id: &str) {
    tap(tree, id);
    settle(tree);
}

#[test]
fn a_split_shows_both_ends_of_one_stack() {
    let mut tree = split_tree(false);
    assert_eq!(text_of(&tree, "listing"), "main");
    assert_eq!(rect_of(&tree, "listing").w, 100);
    assert!(tree.find("depth0").is_none(), "the right stays empty");

    tap(&mut tree, "listing");
    assert!(!tree.transiting(), "showing beside the main screen does not slide");
    settle(&mut tree);
    assert_eq!(text_of(&tree, "listing"), "main-open");
    assert_eq!(rect_of(&tree, "depth0").x, 100);
    assert_eq!(text_of(&tree, "depth0"), "stay", "the first on the right has no back");

    turn(&mut tree, false);
    assert!(tree.find("listing").is_none());
    assert_eq!(rect_of(&tree, "depth0").x, 0);
    assert_eq!(text_of(&tree, "depth0"), "back");

    turn(&mut tree, true);
    assert_eq!(text_of(&tree, "listing"), "main-open");
    go(&mut tree, "back0");
    assert_eq!(text_of(&tree, "listing"), "main");
    assert!(tree.find("depth0").is_none());

    turn(&mut tree, false);
    assert_eq!(text_of(&tree, "listing"), "none");
    assert_eq!(rect_of(&tree, "listing").x, 0);
}

#[test]
fn a_default_detail_stays_out_of_the_stack_until_pushed_from() {
    let mut tree = split_tree(true);
    assert_eq!(text_of(&tree, "listing"), "main");
    assert_eq!(text_of(&tree, "depth0"), "stay");

    turn(&mut tree, false);
    assert!(tree.find("depth0").is_none(), "one pane shows the main screen");
    turn(&mut tree, true);
    assert_eq!(text_of(&tree, "depth0"), "stay");

    go(&mut tree, "deeper0");
    assert_eq!(text_of(&tree, "depth1"), "back");
    assert_eq!(text_of(&tree, "listing"), "main-open");
    go(&mut tree, "back1");
    assert_eq!(text_of(&tree, "depth0"), "stay", "promoted, it does not pop while split");
    assert_eq!(text_of(&tree, "listing"), "main");

    go(&mut tree, "deeper0");
    turn(&mut tree, false);
    go(&mut tree, "back1");
    assert_eq!(text_of(&tree, "depth0"), "back", "one pane walks the whole stack");
    go(&mut tree, "back0");
    assert_eq!(text_of(&tree, "listing"), "none");
}

#[test]
fn a_covered_split_comes_back_whole() {
    let mut tree = split_tree(false);
    go(&mut tree, "listing");
    go(&mut tree, "cover0");
    assert!(tree.find("listing").is_none());
    tree.post(LEAF_ID.load(Relaxed), Erased::new(LeafMsg::Back));
    settle(&mut tree);
    assert_eq!(text_of(&tree, "listing"), "main-open");
    assert_eq!(rect_of(&tree, "depth0").x, 100);
}

#[test]
fn a_color_mode_swaps_the_palette_under_the_same_looks() {
    let mut colors = theme().light;
    colors.surface = Color::rgb(1, 2, 3);
    let dark: &'static Theme = Box::leak(Box::new(Theme { dark: Some(colors), ..*theme() }));
    assert_eq!(dark.palette(ColorMode::Light).surface, theme().light.surface);
    assert_eq!(dark.palette(ColorMode::Dark).surface, Color::rgb(1, 2, 3));
    assert_eq!(theme().palette(ColorMode::Dark).surface, theme().light.surface, "no dark colors stays light");

    assert_eq!(dark.palette(ColorMode::Dark).color(ColorRole::Surface), Color::rgb(1, 2, 3));
    assert_eq!(theme().light.color(ColorRole::Custom(2)), Color::rgb(120, 144, 156));
}

#[test]
fn strips_give_back_the_lent_memory() {
    use devkit::{Lease, StaticLease};

    #[repr(C, align(64))]
    struct Sram([u8; 1024]);
    impl AsMut<[u8]> for Sram {
        fn as_mut(&mut self) -> &mut [u8] {
            &mut self.0
        }
    }
    static SRAM: StaticLease<Sram> = StaticLease::new(Sram([0; 1024]));

    let strips = Strips::new(SRAM.take().unwrap());
    assert!(SRAM.take().is_none());
    let lease = strips.into_inner::<Lease<'static, Sram>>().unwrap();
    drop(lease);
    assert!(SRAM.take().is_some());
    assert!(Strips::allocate(128, Caps::INTERNAL).unwrap().into_inner::<Lease<'static, Sram>>().is_none());
}

thread_local! {
    static MOVER: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

const MOVER_NAMES: [&str; 4] = ["m0", "m1", "m2", "m3"];

#[derive(Clone, Copy)]
enum MoveMsg {
    Push(Option<Transition>),
    Pop(Option<Transition>),
    Replace(Option<Transition>),
    Modal(Option<Transition>),
}

fn asking(nav: Nav<'_>, transition: Option<Transition>) -> Nav<'_> {
    match transition {
        Some(transition) => nav.transition(transition),
        None => nav,
    }
}

struct Mover(usize);

impl Component for Mover {
    type Msg = MoveMsg;

    fn update(&mut self, msg: MoveMsg, cx: &mut Cx<Self>) {
        let next = Mover(self.0 + 1);
        match msg {
            MoveMsg::Push(t) => asking(cx.nav(), t).push(next),
            MoveMsg::Pop(t) => asking(cx.nav(), t).pop(),
            MoveMsg::Replace(t) => asking(cx.nav(), t).replace(next),
            MoveMsg::Modal(t) => asking(cx.nav_of::<Overlay>(), t).push(next),
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<MoveMsg>) -> Element<'a, MoveMsg> {
        let color = if self.0 % 2 == 0 { RED } else { WHITE };
        ui.column([]).grow(1).background(ColorRole::Fixed(color)).id(MOVER_NAMES[self.0])
    }
}

impl Child for Mover {
    fn create(_: &()) -> Self {
        Self(0)
    }

    fn set_props(&mut self, _: &()) {}
}

impl Screen for Mover {
    fn on_appear(&mut self, cx: &mut Cx<Self>) {
        MOVER.with(|m| m.set(cx.handle().id()));
    }
}

fn send_shown(tree: &mut Tree, msg: MoveMsg) {
    tree.post(MOVER.with(|m| m.get()), Erased::new(msg));
    tree.update();
}

#[test]
fn a_move_takes_the_asked_transition_and_pop_plays_it_backwards() {
    let mut tree = Tree::new(Mover(0), SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    send_shown(&mut tree, MoveMsg::Push(Some(Transition::SlideUp)));
    tree.tick(16);
    tree.update();
    let (entering, leaving) = (rect_of(&tree, "m1"), rect_of(&tree, "m0"));
    assert!(entering.x == 0 && entering.y > 0, "comes from below: {entering:?}");
    assert_eq!((leaving.x, leaving.y), (0, 0), "the covered screen stays");

    settle(&mut tree);
    send_shown(&mut tree, MoveMsg::Pop(None));
    tree.tick(16);
    tree.update();
    let (going, back) = (rect_of(&tree, "m1"), rect_of(&tree, "m0"));
    assert!(going.x == 0 && going.y > 0, "goes down again: {going:?}");
    assert_eq!((back.x, back.y), (0, 0));
    settle(&mut tree);
    assert!(tree.find("m1").is_none());

    send_shown(&mut tree, MoveMsg::Push(None));
    tree.tick(16);
    tree.update();
    assert!(rect_of(&tree, "m1").x > 0, "without one, the theme's slide");
}

struct Inner;

struct Holder;

impl Component for Holder {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut Cx<Self>) {}

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        ui.navigator::<Inner, Mover>(()).transition(Transition::None).grow(1)
    }
}

impl Screen for Holder {}

#[test]
fn a_navigator_default_and_replace_change_at_once_unless_asked() {
    let mut tree = Tree::new(Holder, SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    send_shown(&mut tree, MoveMsg::Push(None));
    assert!(!tree.transiting(), "the navigator's default");
    assert!(tree.find("m0").is_none());

    send_shown(&mut tree, MoveMsg::Push(Some(Transition::Slide)));
    assert!(tree.transiting(), "asked over the default");
    settle(&mut tree);

    send_shown(&mut tree, MoveMsg::Replace(None));
    assert!(!tree.transiting(), "replace changes at once");
    assert!(tree.find("m3").is_some() && tree.find("m2").is_none());

    send_shown(&mut tree, MoveMsg::Pop(None));
    assert!(tree.transiting(), "the replaced screen came with a slide");
    settle(&mut tree);
    assert!(tree.find("m1").is_some());

    send_shown(&mut tree, MoveMsg::Replace(Some(Transition::SlideUp)));
    assert!(tree.transiting());
    settle(&mut tree);
    assert!(tree.find("m2").is_some() && tree.find("m1").is_none());
}

struct Asking {
    wide: bool,
}

impl Screen for Asking {}

impl Component for Asking {
    type Msg = ();

    fn update(&mut self, _: (), cx: &mut Cx<Self>) {
        cx.nav_of::<Overlay>().push(Dialog { wide: self.wide, name: TextBuffer::new() });
    }

    fn view<'a>(&'a self, ui: &'a Ui<()>) -> Element<'a, ()> {
        ui.column([])
    }
}

struct Dialog {
    wide: bool,
    name: TextBuffer,
}

impl Screen for Dialog {}

impl Component for Dialog {
    type Msg = TextInput;

    fn update(&mut self, input: TextInput, cx: &mut Cx<Self>) {
        self.name.apply(input);
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<TextInput>) -> Element<'a, TextInput> {
        let modal = widgets::modal(ui, [
            widgets::modal_title(ui, "Rename").id("title"),
            widgets::text_field(ui, &self.name, |input| input).id("name"),
            widgets::modal_actions(ui, [0, 1, 2].map(|i| ui.column([]).height(40).id(["a0", "a1", "a2"][i]))),
        ]);
        if self.wide { modal.part(Part::Panel, |panel| panel.width(200).id("panel")) } else { modal.part(Part::Panel, |panel| panel.id("panel")) }
    }
}

fn asked(wide: bool) -> Tree {
    asked_with(wide, |_| {})
}

fn asked_with(wide: bool, setup: impl FnOnce(&mut Tree)) -> Tree {
    let mut tree = Tree::new(Asking { wide }, SCREEN, Caps::INTERNAL, theme());
    setup(&mut tree);
    run(&mut tree);
    tree.post(tree.root_id, Erased::new(()));
    run(&mut tree);
    tree.tick(1000);
    run(&mut tree);
    tree
}

#[test]
fn a_modal_is_centered_and_keeps_its_margins() {
    let tree = asked(false);
    let panel = rect_of(&tree, "panel");
    assert_eq!((panel.x, panel.w), (24, SCREEN.w - 48), "the theme's width does not fit, so the margins stay");
    assert_eq!(panel.y + panel.h / 2, SCREEN.h / 2);
    let actions = ["a0", "a1", "a2"].map(|id| rect_of(&tree, id));
    assert!(actions.iter().all(|a| a.w == actions[0].w && a.y == actions[0].y), "{actions:?}");
    let tree = asked(true);
    let panel = rect_of(&tree, "panel");
    assert_eq!((panel.x, panel.w), (100, 200), "a part sets its own width");
}

#[test]
fn a_modal_moves_above_the_keyboard() {
    let mut tree = asked(false);
    tree.add_keyboard(crate::keyboard::KeyboardMode::Auto);
    run(&mut tree);
    tap(&mut tree, "name");
    settle(&mut tree);
    let keyboard = TEXT_KEYS.height;
    let panel = rect_of(&tree, "panel");
    assert_eq!(panel.y + panel.h / 2, (SCREEN.h - keyboard) / 2, "centered in what the keyboard leaves");
}

#[test]
fn insets_are_what_the_planes_above_cover() {
    let mut tree = asked_with(false, |tree| {
        tree.add_chrome(TopChrome { height: 40 });
        tree.add_keyboard(crate::keyboard::KeyboardMode::Auto);
    });
    let chrome = tree.chrome[0];
    let dialog = tree.nodes[tree.find("panel").unwrap() as usize].owner;
    assert_eq!((tree.context(0).insets.top, tree.context(chrome).insets.top, tree.context(dialog).insets.top), (40, 0, 0));
    assert_eq!(rect_of(&tree, "panel").y + rect_of(&tree, "panel").h / 2, SCREEN.h / 2, "the overlay is above the chrome");
    tap(&mut tree, "name");
    settle(&mut tree);
    let keyboard = TEXT_KEYS.height;
    assert_eq!([0, chrome, dialog].map(|slot| tree.context(slot).insets.bottom), [keyboard; 3]);
}

#[test]
fn modals_change_at_once_whatever_is_asked() {
    let mut tree = Tree::new(Mover(0), SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    send_shown(&mut tree, MoveMsg::Modal(Some(Transition::Slide)));
    send_shown(&mut tree, MoveMsg::Modal(Some(Transition::Slide)));
    assert!(!tree.transiting());
    assert!(tree.find("m2").is_some());
}

fn zoom_frame(tree: &mut Tree, cache: &mut devkit_gfx::GlyphCache<Caps>) -> Vec<u16> {
    let mut effects = crate::paint::Effects::new(Caps::INTERNAL);
    tree.update();
    tree.capture_zooms(&mut effects, cache, None);
    let mut list = devkit_gfx::DisplayList::new_in(Caps::INTERNAL);
    tree.paint(&mut effects, &[SCREEN], &mut list);
    let mut pixels = vec![0u16; SCREEN.area() as usize];
    let mut prepared = devkit_gfx::Prepared::new_in(Caps::INTERNAL);
    prepared.prepare(&list, &[SCREEN], cache);
    let surface = unsafe { devkit_gfx::Surface::new(pixels.as_mut_ptr().cast(), SCREEN.w as usize * 2, SCREEN) };
    devkit_gfx::draw::<devkit_gfx::Rgb565, _, _, _>(&list, &prepared, &mut devkit_gfx::ClipStore::new_in(Caps::INTERNAL), &surface);
    pixels
}

#[test]
fn a_zoom_draws_pictures_from_the_old_screen_to_the_new_one() {
    const RED_565: u16 = 0xf800;
    const WHITE_565: u16 = 0xffff;
    let mut cache = devkit_gfx::GlyphCache::new_in(16 * 1024, Caps::INTERNAL);
    let mut tree = Tree::new(Mover(0), SCREEN, Caps::INTERNAL, theme());
    settle(&mut tree);
    send_shown(&mut tree, MoveMsg::Push(Some(Transition::Zoom)));
    let at = |pixels: &[u16], x: i16, y: i16| pixels[y as usize * SCREEN.w as usize + x as usize];
    let first = zoom_frame(&mut tree, &mut cache);
    assert_eq!(at(&first, 200, 300), RED_565, "the old screen first, in front");
    assert_eq!(at(&first, 0, 0), RED_565, "covering the pane");

    tree.tick(1000);
    assert!(tree.transiting(), "taking the pictures is not part of the time");
    tree.tick(150);
    let middle = zoom_frame(&mut tree, &mut cache);
    let center = at(&middle, 200, 300);
    assert!(center != RED_565 && center != WHITE_565, "both show through: {center:04x}");
    assert_ne!(at(&middle, 0, 0), center, "the new screen is not in the corner yet");
    let edge = at(&middle, 10, 2);
    assert!(edge >> 5 & 0x3f > 0, "the background, not black, shows through above the new screen: {edge:04x}");

    tree.tick(140);
    let late = zoom_frame(&mut tree, &mut cache);
    assert_eq!(at(&late, 200, 300) & 0x07e0, WHITE_565 & 0x07e0, "the new screen in the end");
    settle(&mut tree);
    assert!(tree.find("m0").is_none() && tree.find("m1").is_some());
}

#[test]
fn a_picture_takes_every_other_pixel_at_half_size() {
    let mut picture = crate::zoom::Picture::allocate(Rect::new(0, 0, 4, 2)).unwrap();
    let band = [1, 2, 3, 4, 5, 6, 7, 8];
    picture.sample(&band, 4, 2, 0);
    assert_eq!((picture.width, picture.height), (2, 1));
    let pixels = unsafe { core::slice::from_raw_parts(picture.as_mut_ptr(), 2) };
    assert_eq!(pixels, [6, 8], "the pixel each output centre falls on");
}

#[derive(Clone, Copy)]
enum PullMsg {
    Refresh,
    Begin,
    Done,
    Grow,
}

struct Puller {
    count: usize,
    bounce: bool,
    pull: bool,
    refreshing: bool,
    refreshes: &'static core::cell::Cell<u32>,
}

impl Puller {
    fn new(count: usize) -> Self {
        Self { count, bounce: true, pull: false, refreshing: false, refreshes: Box::leak(Box::new(core::cell::Cell::new(0))) }
    }
}

impl Component for Puller {
    type Msg = PullMsg;

    fn update(&mut self, msg: PullMsg, cx: &mut Cx<Self>) {
        match msg {
            PullMsg::Refresh => {
                self.refreshes.set(self.refreshes.get() + 1);
                self.refreshing = true;
            }
            PullMsg::Begin => self.refreshing = true,
            PullMsg::Done => self.refreshing = false,
            PullMsg::Grow => self.count += 1,
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<PullMsg>) -> Element<'a, PullMsg> {
        let list = ui.list(0, self.count, 40).grow(1).bounce(self.bounce).id("list");
        if self.pull { list.pull(60, self.refreshing, PullMsg::Refresh) } else { list }
    }

    fn list_row<'a>(&'a self, _: ListId, _: usize, ui: &'a Ui<PullMsg>) -> Element<'a, PullMsg> {
        ui.column([]).height(40)
    }
}

impl Screen for Puller {}

fn puller(puller: Puller) -> Tree {
    let mut tree = Tree::new(puller, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    tree
}

fn pull_list(tree: &mut Tree, by: i16, at: u32) {
    let r = rect_of(tree, "list");
    let (x, y) = (r.x + 10, r.y + 100);
    for step in 0..=10 {
        tree.touch(&[(0, (x, y + by * step / 10))], at + step as u32 * 16);
    }
    tree.touch(&[(0, (x, y + by))], at + 400);
    tree.touch(&[], at + 410);
}

fn throw_list(tree: &mut Tree, at: u32) {
    let r = rect_of(tree, "list");
    let x = r.x + 10;
    for (i, y) in [500, 460, 420, 380, 340].into_iter().enumerate() {
        tree.touch(&[(0, (x, r.y + y))], at + i as u32 * 10);
    }
    tree.touch(&[], at + 45);
}

fn rest(tree: &mut Tree) -> Vec<i32> {
    let mut seen = Vec::new();
    for _ in 0..400 {
        if !tree.animating() {
            break;
        }
        tree.tick(16);
        seen.push(offset_of(tree, "list"));
    }
    assert!(!tree.animating(), "still moving at {}", offset_of(tree, "list"));
    run(tree);
    seen
}

#[test]
fn a_drag_past_the_start_stretches_and_springs_back() {
    let mut tree = puller(Puller::new(100));
    pull_list(&mut tree, 300, 0);
    let past = offset_of(&tree, "list");
    assert!(past < -60 && past > -300 * 55 / 100, "{past}");
    assert!(tree.animating());
    rest(&mut tree);
    assert_eq!(offset_of(&tree, "list"), 0);
}

#[test]
fn dragging_back_retraces_the_stretch() {
    let path = |ys: &[i16]| {
        let mut tree = puller(Puller::new(100));
        let r = rect_of(&tree, "list");
        for (i, &y) in ys.iter().enumerate() {
            tree.touch(&[(0, (r.x + 10, r.y + 200 + y))], i as u32 * 16);
        }
        offset_of(&tree, "list")
    };
    assert_eq!(path(&[0, 200, 300, 200, 100, 50]), path(&[0, 50]));
    assert!(path(&[0, 50]) < 0);
    assert_eq!(path(&[0, 200, 300, 100, -100]), 100, "one to one again inside");
}

#[test]
fn a_fling_into_the_end_overshoots_and_comes_back() {
    let mut tree = puller(Puller::new(20));
    throw_list(&mut tree, 0);
    let seen = rest(&mut tree);
    let end = 20 * 40 - 600;
    assert!(seen.iter().any(|&o| o > end), "{seen:?}");
    assert_eq!(offset_of(&tree, "list"), end);
}

#[test]
fn without_bounce_the_ends_hold() {
    let mut tree = puller(Puller { bounce: false, ..Puller::new(20) });
    pull_list(&mut tree, 300, 0);
    assert_eq!(offset_of(&tree, "list"), 0);
    assert!(!tree.animating());
    throw_list(&mut tree, 1000);
    let seen = rest(&mut tree);
    assert!(seen.iter().all(|&o| o <= 200), "{seen:?}");
    assert_eq!(offset_of(&tree, "list"), 200);
}

#[test]
fn content_that_fits_does_not_bounce() {
    let mut tree = puller(Puller::new(5));
    pull_list(&mut tree, 300, 0);
    assert_eq!(offset_of(&tree, "list"), 0);
    pull_list(&mut tree, -300, 1000);
    assert_eq!(offset_of(&tree, "list"), 0);
}

#[test]
fn a_press_catches_the_spring_and_a_lift_lets_it_go() {
    let mut tree = puller(Puller::new(100));
    pull_list(&mut tree, 300, 0);
    tree.tick(16);
    let r = rect_of(&tree, "list");
    tree.touch(&[(0, (r.x + 10, r.y + 300))], 500);
    let held = offset_of(&tree, "list");
    assert!(held < 0);
    tree.tick(16);
    assert_eq!(offset_of(&tree, "list"), held);
    tree.touch(&[], 600);
    rest(&mut tree);
    assert_eq!(offset_of(&tree, "list"), 0);
}

#[test]
fn a_layout_while_springing_back_does_not_snap() {
    let mut tree = puller(Puller::new(100));
    pull_list(&mut tree, 300, 0);
    tree.tick(16);
    tree.post(tree.root_id, Erased::new(PullMsg::Grow));
    run(&mut tree);
    assert!(offset_of(&tree, "list") < -20);
    rest(&mut tree);
    assert_eq!(offset_of(&tree, "list"), 0);
}

#[test]
fn rows_follow_past_the_start() {
    let mut tree = puller(Puller::new(100));
    let r = rect_of(&tree, "list");
    tree.touch(&[(0, (r.x + 10, r.y + 100))], 0);
    tree.touch(&[(0, (r.x + 10, r.y + 300))], 16);
    tree.touch(&[(0, (r.x + 10, r.y + 400))], 32);
    run(&mut tree);
    let offset = offset_of(&tree, "list");
    let NodeContent::List(list) = &tree.nodes[tree.find("list").unwrap() as usize].content else { panic!() };
    let (index, row) = list.rows[0];
    assert_eq!(index, 0);
    assert_eq!(i32::from(tree.nodes[row as usize].screen.y), i32::from(r.y) - offset);
    tree.touch(&[], 400);
}

#[test]
fn a_pinch_taking_the_finger_springs_back() {
    let mut tree = Tree::new(Photos { pinches: 0 }, SCREEN, Caps::INTERNAL, theme());
    run(&mut tree);
    let r = rect_of(&tree, "photos");
    let x = r.x + 50;
    tree.touch(&[(0, (x, r.y + 100))], 0);
    tree.touch(&[(0, (x, r.y + 200))], 10);
    tree.touch(&[(0, (x, r.y + 300))], 20);
    assert!(offset_of(&tree, "photos") < 0);
    tree.touch(&[(0, (x, r.y + 300)), (1, (x + 40, r.y + 300))], 30);
    assert!(tree.animating());
    for _ in 0..100 {
        tree.tick(16);
    }
    assert_eq!(offset_of(&tree, "photos"), 0);
    tree.touch(&[], 2000);
}

#[test]
fn turning_the_screen_puts_a_stretched_list_back() {
    let mut tree = puller(Puller::new(100));
    let r = rect_of(&tree, "list");
    tree.touch(&[(0, (r.x + 10, r.y + 100))], 0);
    tree.touch(&[(0, (r.x + 10, r.y + 300))], 16);
    tree.touch(&[(0, (r.x + 10, r.y + 400))], 32);
    assert!(offset_of(&tree, "list") < 0);
    tree.set_area(Rect::new(0, 0, 600, 400));
    run(&mut tree);
    assert_eq!(offset_of(&tree, "list"), 0);
    tree.touch(&[], 400);
}

#[test]
fn a_pull_past_its_height_refreshes_and_stays_open_until_done() {
    let refreshes = Box::leak(Box::new(core::cell::Cell::new(0)));
    let mut tree = puller(Puller { pull: true, refreshes, ..Puller::new(100) });
    pull_list(&mut tree, 300, 0);
    run(&mut tree);
    assert_eq!(refreshes.get(), 1);
    rest(&mut tree);
    assert_eq!(offset_of(&tree, "list"), -60);
    tree.post(tree.root_id, Erased::new(PullMsg::Grow));
    run(&mut tree);
    rest(&mut tree);
    assert_eq!(offset_of(&tree, "list"), -60, "a rebuild still refreshing keeps it open");
    tree.post(tree.root_id, Erased::new(PullMsg::Done));
    run(&mut tree);
    rest(&mut tree);
    assert_eq!(offset_of(&tree, "list"), 0);
    assert_eq!(refreshes.get(), 1);
}

#[test]
fn a_short_pull_springs_back_without_refreshing() {
    let refreshes = Box::leak(Box::new(core::cell::Cell::new(0)));
    let mut tree = puller(Puller { pull: true, refreshes, ..Puller::new(100) });
    pull_list(&mut tree, 80, 0);
    run(&mut tree);
    rest(&mut tree);
    assert_eq!(refreshes.get(), 0);
    assert_eq!(offset_of(&tree, "list"), 0);
}

#[test]
fn an_empty_list_pulls_even_without_bounce() {
    let refreshes = Box::leak(Box::new(core::cell::Cell::new(0)));
    let mut tree = puller(Puller { pull: true, bounce: false, refreshes, ..Puller::new(0) });
    pull_list(&mut tree, 300, 0);
    run(&mut tree);
    assert_eq!(refreshes.get(), 1);
    rest(&mut tree);
    assert_eq!(offset_of(&tree, "list"), -60);
    pull_list(&mut tree, -300, 1000);
    assert_eq!(offset_of(&tree, "list"), 0, "no stretch past the end without bounce");
}

#[test]
fn refreshing_from_the_view_opens_at_the_start_only() {
    let mut tree = puller(Puller { pull: true, ..Puller::new(100) });
    tree.post(tree.root_id, Erased::new(PullMsg::Begin));
    run(&mut tree);
    rest(&mut tree);
    assert_eq!(offset_of(&tree, "list"), -60);
    tree.post(tree.root_id, Erased::new(PullMsg::Done));
    run(&mut tree);
    rest(&mut tree);
    pull_list(&mut tree, -200, 1000);
    rest(&mut tree);
    let away = offset_of(&tree, "list");
    tree.post(tree.root_id, Erased::new(PullMsg::Begin));
    run(&mut tree);
    assert!(!tree.animating());
    assert_eq!(offset_of(&tree, "list"), away);
}

#[test]
fn a_pull_is_lighter_up_to_its_height_and_retraces() {
    let path = |pull: bool, ys: &[i16]| {
        let mut tree = puller(Puller { pull, ..Puller::new(100) });
        let r = rect_of(&tree, "list");
        for (i, &y) in ys.iter().enumerate() {
            tree.touch(&[(0, (r.x + 10, r.y + 200 + y))], i as u32 * 16);
        }
        -offset_of(&tree, "list")
    };
    assert!(path(true, &[0, 40]) > path(false, &[0, 40]));
    let (near, far) = (path(true, &[0, 200]), path(true, &[0, 400]));
    assert!(near > 60 && far - near < near, "{near} {far}");
    assert_eq!(path(true, &[0, 300, 400, 100]), path(true, &[0, 100]));
}

thread_local! {
    static CARD: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

struct Below {
    taps: &'static AtomicU32,
}

#[derive(Clone, Copy)]
enum BelowMsg {
    Tap,
    Open(bool),
}

impl Screen for Below {}

impl Component for Below {
    type Msg = BelowMsg;

    fn update(&mut self, msg: BelowMsg, cx: &mut Cx<Self>) {
        match msg {
            BelowMsg::Tap => {
                self.taps.fetch_add(1, Relaxed);
            }
            BelowMsg::Open(dismiss) => cx.nav_of::<Overlay>().push(Card { dismiss, inner: true }),
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<BelowMsg>) -> Element<'a, BelowMsg> {
        ui.column([
            ui.row([]).size(400, 300).id("below").on_tap(BelowMsg::Tap),
            ui.row([]).size(40, 40).id("open").on_tap(BelowMsg::Open(true)),
            ui.row([]).size(40, 40).id("open-kept").on_tap(BelowMsg::Open(false)),
        ])
    }
}

struct Card {
    dismiss: bool,
    inner: bool,
}

#[derive(Clone, Copy)]
enum CardMsg {
    Close,
    Drop,
    Again,
}

impl Component for Card {
    type Msg = CardMsg;

    fn update(&mut self, msg: CardMsg, cx: &mut Cx<Self>) {
        match msg {
            CardMsg::Close => cx.nav_of::<Overlay>().pop(),
            CardMsg::Drop => self.inner = false,
            CardMsg::Again => cx.nav_of::<Overlay>().push(Card { dismiss: self.dismiss, inner: true }),
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<CardMsg>) -> Element<'a, CardMsg> {
        let inner = if self.inner { ui.row([]).size(40, 40).background(RED).key(1).id("inner").appear(Presence::fade()) } else { ui.empty() };
        ui.column([ui
            .column([inner, ui.row([]).size(200, 40).id("in-card")])
            .background(WHITE)
            .absolute(Some(100), Some(400), None, None)
            .id("card")
            .appear(Presence::from_edge(Edge::Bottom))
            .disappear(Presence::fade())])
    }
}

impl Screen for Card {
    fn on_appear(&mut self, cx: &mut Cx<Self>) {
        CARD.with(|c| c.set(cx.handle().id()));
    }

    fn scrim_tap(&self) -> Option<CardMsg> {
        self.dismiss.then_some(CardMsg::Close)
    }
}

fn send_card(tree: &mut Tree, msg: CardMsg) {
    tree.post(CARD.with(|c| c.get()), Erased::new(msg));
    tree.update();
}

fn card_tree(theme: &'static Theme) -> (Tree, &'static AtomicU32) {
    let taps = Box::leak(Box::new(AtomicU32::new(0)));
    let mut tree = Tree::new(Below { taps }, SCREEN, Caps::INTERNAL, theme);
    settle(&mut tree);
    (tree, taps)
}

/// Finds what `tree.find` skips: nodes off the screen.
fn node_of(tree: &Tree, from: NodeId, id: &str) -> Option<NodeId> {
    let node = &tree.nodes[from as usize];
    if node.id == Some(id) {
        return Some(from);
    }
    let mut child = node.first_child;
    while child != NONE {
        if let Some(found) = node_of(tree, child, id) {
            return Some(found);
        }
        child = tree.nodes[child as usize].next;
    }
    None
}

fn count(tree: &Tree, from: NodeId, id: &str) -> usize {
    let node = &tree.nodes[from as usize];
    let mut found = usize::from(node.id == Some(id));
    let mut child = node.first_child;
    while child != NONE {
        found += count(tree, child, id);
        child = tree.nodes[child as usize].next;
    }
    found
}

#[test]
fn a_screen_on_the_overlay_comes_from_its_edge_over_a_scrim_that_follows_it() {
    let (mut tree, _) = card_tree(theme());
    tap(&mut tree, "open");
    let card = node_of(&tree, tree.root, "card").unwrap();
    assert_eq!(tree.nodes[card as usize].pose.unwrap().offset, (0, 200), "just below the screen");
    assert_eq!(tree.scrim, 0);
    tree.tick(100);
    tree.update();
    let (offset, scrim) = (tree.nodes[card as usize].pose.unwrap().offset.1, tree.scrim);
    assert!(offset > 0 && offset < 200, "{offset}");
    assert!(scrim > 0 && scrim < 128, "{scrim}");
    settle(&mut tree);
    assert!(tree.nodes[card as usize].pose.is_none());
    assert_eq!(rect_of(&tree, "card").y, 400);
    assert_eq!(tree.scrim, 128);
}

#[test]
fn the_scrim_keeps_touches_from_below_and_sends_what_the_screen_asks() {
    let (mut tree, taps) = card_tree(theme());
    tap(&mut tree, "open-kept");
    settle(&mut tree);
    tap_at(&mut tree, (10, 10));
    settle(&mut tree);
    assert_eq!(taps.load(Relaxed), 0);
    assert!(tree.find("card").is_some(), "no scrim_tap, nothing closes");

    send_card(&mut tree, CardMsg::Close);
    settle(&mut tree);
    tap(&mut tree, "open");
    settle(&mut tree);
    tap_at(&mut tree, (10, 10));
    assert!(tree.find("card").is_some(), "kept while it disappears");
    tree.tick(100);
    tree.update();
    assert!(tree.scrim > 0 && tree.scrim < 128, "{}", tree.scrim);
    tap_at(&mut tree, (10, 10));
    assert_eq!(taps.load(Relaxed), 0, "still covered");
    settle(&mut tree);
    assert!(tree.find("card").is_none());
    assert_eq!(tree.scrim, 0);
    tap_at(&mut tree, (10, 10));
    assert_eq!(taps.load(Relaxed), 1);
}

#[test]
fn a_push_on_the_overlay_replaces_what_shows_at_once() {
    let (mut tree, _) = card_tree(theme());
    tap(&mut tree, "open");
    settle(&mut tree);
    send_card(&mut tree, CardMsg::Again);
    assert_eq!(count(&tree, tree.root, "card"), 1);
    assert_eq!(tree.scrim, 0, "the scrim comes again with the new screen");
    settle(&mut tree);
    assert_eq!(tree.scrim, 128);
}

#[test]
fn an_element_taken_out_stays_until_it_disappears_out_of_the_flow() {
    let (mut tree, _) = card_tree(theme());
    tap(&mut tree, "open");
    settle(&mut tree);
    let inner = rect_of(&tree, "inner");
    assert_eq!(rect_of(&tree, "in-card").y, inner.bottom());
    send_card(&mut tree, CardMsg::Drop);
    let left = tree.find("inner").unwrap();
    assert!(tree.nodes[left as usize].leaving);
    assert_eq!(rect_of(&tree, "inner"), inner);
    assert_eq!(rect_of(&tree, "in-card").y, inner.y);
    tree.tick(100);
    tree.update();
    let opacity = tree.nodes[left as usize].pose.unwrap().opacity;
    assert!(opacity > 0 && opacity < 255, "{opacity}");
    settle(&mut tree);
    assert!(tree.find("inner").is_none());
}

#[test]
fn a_clear_scrim_damages_only_what_draws() {
    let clear: &'static Theme = Box::leak(Box::new(Theme { widgets: Widgets { overlay: OverlayTheme { scrim: ColorRole::Fixed(Color::rgba(0, 0, 0, 0)) }, ..theme().widgets }, ..*theme() }));
    let (mut tree, _) = card_tree(clear);
    let mut damage = Vec::new();
    let mut play = |tree: &mut Tree| {
        for _ in 0..40 {
            tree.update();
            damage.extend(tree.take_damage());
            tree.tick(16);
        }
    };
    tap(&mut tree, "open");
    play(&mut tree);
    send_card(&mut tree, CardMsg::Close);
    play(&mut tree);
    assert!(tree.find("card").is_none());
    assert!(!damage.is_empty());
    for rect in damage.iter().filter(|r| !r.is_empty()) {
        assert!(rect.w <= 200 && rect.y >= 300, "{rect:?} in {damage:?}");
    }
    assert_eq!(tree.scrim, 0);
}
