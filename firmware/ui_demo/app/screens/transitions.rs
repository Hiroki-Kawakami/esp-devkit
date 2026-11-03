/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use devkit_ui::widgets::{list_item, segmented, top_bar};
use devkit_ui::{Child, ColorRole, Component, Cx, Element, Id, Nav, Root, Screen, Transition, Ui, id};

use crate::resources::colors;

/// Marks the navigator of the transition pages.
pub struct Stage;

const DEFAULTS: [(&str, Option<Transition>); 5] = [
    ("Theme", None),
    ("None", Some(Transition::None)),
    ("Slide", Some(Transition::Slide)),
    ("Up", Some(Transition::SlideUp)),
    ("Zoom", Some(Transition::Zoom)),
];

#[derive(Clone, Copy)]
pub enum TransitionMsg {
    Back,
    Default(usize),
}

pub struct TransitionDemo {
    default: usize,
}

impl TransitionDemo {
    pub(crate) fn new() -> Self {
        Self { default: 0 }
    }
}

impl Component for TransitionDemo {
    type Msg = TransitionMsg;

    fn update(&mut self, msg: TransitionMsg, cx: &mut Cx<Self>) {
        match msg {
            TransitionMsg::Back => cx.nav_of::<Root>().pop(),
            TransitionMsg::Default(index) => {
                self.default = index;
                cx.rebuild();
            }
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<TransitionMsg>) -> Element<'a, TransitionMsg> {
        let theme = ui.theme();
        let options: Vec<_> = DEFAULTS.iter().enumerate().map(|(i, (name, _))| (*name, TransitionMsg::Default(i))).collect();
        let mut stage = ui.navigator::<Stage, Page>(()).grow(1).id(id!("stage"));
        if let Some(transition) = DEFAULTS[self.default].1 {
            stage = stage.transition(transition);
        }
        ui.column([
            top_bar(ui, Some(TransitionMsg::Back), "Transitions", ui.empty()),
            ui.column([segmented(ui, &options, self.default).id(id!("transition-default"))]).padding(theme.widgets.unit.padding),
            stage,
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(ui.colors().background)
    }
}

impl Screen for TransitionDemo {}

#[derive(Clone, Copy)]
pub enum PageMsg {
    Push(Option<Transition>),
    Replace(Option<Transition>),
    Cover(Transition),
    Back,
}

const MOVES: [(&str, &str, PageMsg); 9] = [
    ("Push", "push", PageMsg::Push(None)),
    ("Push: Slide", "push-slide", PageMsg::Push(Some(Transition::Slide))),
    ("Push: Up", "push-up", PageMsg::Push(Some(Transition::SlideUp))),
    ("Push: Zoom", "push-zoom", PageMsg::Push(Some(Transition::Zoom))),
    ("Push: None", "push-none", PageMsg::Push(Some(Transition::None))),
    ("Replace", "replace", PageMsg::Replace(None)),
    ("Replace: Zoom", "replace-zoom", PageMsg::Replace(Some(Transition::Zoom))),
    ("Whole screen: Zoom", "cover-zoom", PageMsg::Cover(Transition::Zoom)),
    ("Back", "page-back", PageMsg::Back),
];

fn asking(nav: Nav<'_>, transition: Option<Transition>) -> Nav<'_> {
    match transition {
        Some(transition) => nav.transition(transition),
        None => nav,
    }
}

pub struct Page {
    depth: u32,
    /// Shown on the root navigator, under the chrome.
    covers: bool,
    title: String,
}

impl Page {
    fn new(depth: u32, covers: bool) -> Self {
        Self { depth, covers, title: format!("Page {depth}") }
    }
}

impl Component for Page {
    type Msg = PageMsg;

    fn update(&mut self, msg: PageMsg, cx: &mut Cx<Self>) {
        let next = Page::new(self.depth + 1, self.covers);
        match msg {
            PageMsg::Push(transition) => asking(cx.nav(), transition).push(next),
            PageMsg::Replace(transition) => asking(cx.nav(), transition).replace(next),
            PageMsg::Cover(transition) => cx.nav_of::<Root>().transition(transition).push(Page::new(self.depth + 1, true)),
            PageMsg::Back => cx.nav().pop(),
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<PageMsg>) -> Element<'a, PageMsg> {
        let theme = ui.theme();
        let c = ui.colors();
        let tint = if self.depth % 2 == 0 { ColorRole::from(c.primary) } else { colors::MUTED };
        let header = ui
            .column([ui.text(theme.fonts.title, &self.title).foreground(c.surface)])
            .padding(theme.widgets.unit.padding * 2)
            .background(tint);
        let items: Vec<_> = MOVES.iter().map(|(name, id, msg)| list_item(ui, None, name, None, None, *msg).id(Id::named(*id))).collect();
        let top = if self.covers { ui.insets().top } else { 0 };
        ui.column([header, ui.scroll(ui.column(items)).grow(1)])
            .padding_each(0, top, 0, 0)
            .background(c.background)
    }
}

impl Child for Page {
    fn create(_: &()) -> Self {
        Self::new(0, false)
    }

    fn set_props(&mut self, _: &()) {}
}

impl Screen for Page {}
