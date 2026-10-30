/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;

use devkit_gfx::format::{self, FontHeader, GlyphRecord};
use devkit_gfx::{Color, Font, FontChain, Rect};
use idf::heap::Caps;

use crate::*;
use crate::node::NodeContent;
use crate::tree::Tree;

const SCREEN: Rect = Rect::new(0, 0, 400, 600);
const WHITE: Color = Color::rgb(255, 255, 255);
const RED: Color = Color::rgb(255, 0, 0);

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

fn run(tree: &mut Tree) -> Vec<Rect> {
    tree.update();
    tree.take_damage()
}

fn rect_of(tree: &Tree, id: &str) -> Rect {
    tree.nodes[tree.find(id).unwrap() as usize].screen
}

fn tap(tree: &mut Tree, id: &str) {
    let point = tree.tap_point(id).unwrap();
    tree.touch(Some(point), 0);
    tree.touch(None, 10);
    tree.update();
}

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
                ui.column([]).size(40, 30).background(RED).id("button").on_tap(Msg::Inc).pressed_background(WHITE),
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
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL);
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

#[test]
fn tap_runs_the_handler_and_changes_only_what_changed() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL);
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
    let mut tree = Tree::new(sample, SCREEN, Caps::INTERNAL);
    run(&mut tree);
    let before = views.get();
    let point = tree.tap_point("button").unwrap();
    tree.touch(Some(point), 0);
    let damage = run(&mut tree);
    assert_eq!(views.get(), before);
    assert!(damage.contains(&Rect::new(350, 5, 40, 30)));
}

#[test]
fn keys_keep_nodes_when_siblings_move() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL);
    run(&mut tree);
    let (a, b) = (tree.find("a").unwrap(), tree.find("b").unwrap());
    tree.post(0, crate::erased::Erased::new(Msg::Reorder));
    run(&mut tree);
    assert_eq!(tree.find("a"), Some(a));
    assert_eq!(tree.find("b"), Some(b));
    assert!(rect_of(&tree, "b").x < rect_of(&tree, "a").x);
}

#[test]
fn removed_nodes_damage_their_area_and_are_reused() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL);
    run(&mut tree);
    tree.post(0, crate::erased::Erased::new(Msg::Toggle));
    run(&mut tree);
    let panel = rect_of(&tree, "panel");
    assert_eq!(panel, Rect::new(290, 20, 100, 100));
    let nodes = tree.nodes.len();
    tree.post(0, crate::erased::Erased::new(Msg::Toggle));
    let damage = run(&mut tree);
    assert!(damage.contains(&panel));
    assert!(tree.find("panel").is_none());
    tree.post(0, crate::erased::Erased::new(Msg::Toggle));
    run(&mut tree);
    assert_eq!(tree.nodes.len(), nodes);
}

#[test]
fn child_state_survives_and_events_reach_the_parent() {
    let sample = Sample::new();
    let views = sample.views;
    let mut tree = Tree::new(sample, SCREEN, Caps::INTERNAL);
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
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL);
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
    tree.touch(Some((x, viewport.y + 200)), 0);
    tree.touch(Some((x, viewport.y + 100)), 16);
    tree.touch(Some((x, viewport.y + 20)), 32);
    let damage = run(&mut tree);
    assert!(damage.iter().all(|r| *r == viewport));
    let shown = rows(&tree);
    assert!(shown[0] > 0);
    assert!(tree.nodes.len() <= nodes + 8);
    tree.touch(None, 40);
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
    let mut tree = Tree::new(Label { font: chain(), value: 2, revision: 0, rows }, SCREEN, Caps::INTERNAL);
    run(&mut tree);
    let built = rows.get();
    assert!(built > 0);
    tree.post(0, crate::erased::Erased::new(Set(9)));
    let damage = run(&mut tree);
    assert_eq!(rows.get(), built);
    assert!(damage.iter().all(|r| r.h <= 12), "{damage:?}");
    let NodeContent::Text(text) = &tree.nodes[tree.find("label").unwrap() as usize].content else { panic!() };
    assert_eq!(text.run.width(), 72);
}

#[test]
fn a_fling_survives_a_slow_last_sample_but_not_a_pause() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL);
    run(&mut tree);
    let viewport = rect_of(&tree, "list");
    let x = viewport.x + 10;
    let drag = |tree: &mut Tree, last_ms: u32, up_ms: u32| {
        tree.touch(Some((x, viewport.y + 300)), 0);
        tree.touch(Some((x, viewport.y + 260)), 10);
        tree.touch(Some((x, viewport.y + 220)), 20);
        tree.touch(Some((x, viewport.y + 180)), 30);
        tree.touch(Some((x, viewport.y + 178)), last_ms);
        tree.touch(Some((x, viewport.y + 178)), last_ms + 10);
        tree.touch(None, up_ms);
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
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL);
    run(&mut tree);
    let viewport = rect_of(&tree, "list");
    let x = viewport.x + 10;
    tree.touch(Some((x, viewport.y + 300)), 0);
    tree.touch(Some((x, viewport.y + 260)), 10);
    tree.touch(Some((x, viewport.y + 220)), 20);
    tree.touch(Some((x, viewport.y + 220)), 30);
    tree.touch(Some((x, viewport.y + 220)), 40);
    tree.touch(None, 45);
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
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL);
    run(&mut tree);
    let point = tree.tap_point("row2").unwrap();
    tree.touch(Some(point), 0);
    tree.touch(Some((point.0, point.1 - 40)), 16);
    tree.touch(None, 500);
    tree.update();
    assert!(tree.find("panel").is_none());
}

#[test]
fn covered_nodes_refuse_harness_taps_and_invoke_goes_direct() {
    let mut tree = Tree::new(Sample::new(), SCREEN, Caps::INTERNAL);
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
    let mut tree = Tree::new(Grid(chain()), Rect::new(0, 0, 320, 200), Caps::INTERNAL);
    run(&mut tree);
    assert_eq!(rect_of(&tree, "c1"), Rect::new(5 + 106, 0, 96, 20));
    assert_eq!(rect_of(&tree, "c3"), Rect::new(5, 30, 96, 96));
    assert_eq!(rect_of(&tree, "clipped").w, 50);
    assert_eq!(rect_of(&tree, "cover"), Rect::new(0, 0, 320, 200));
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
