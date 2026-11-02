/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use devkit_ui::widgets::{button, list_item, modal, toast, top_bar};
use devkit_ui::{Component, Cx, Edge, Element, Overlay, Presence, Root, Screen, Ui};

use crate::resources::colors;

#[derive(Clone, Copy)]
pub enum Kind {
    Modal,
    Kept,
    ScaleOut,
    ShrinkIn,
    Sheet,
    Side,
}

const KINDS: [(&str, &str, Kind); 6] = [
    ("Modal", "open-modal", Kind::Modal),
    ("Modal: scrim does nothing", "open-kept", Kind::Kept),
    ("Modal: at once in, scale out", "open-scale-out", Kind::ScaleOut),
    ("Modal: shrink in", "open-shrink-in", Kind::ShrinkIn),
    ("Bottom sheet", "open-sheet", Kind::Sheet),
    ("Side panel", "open-side", Kind::Side),
];

#[derive(Clone, Copy)]
pub enum PresenceMsg {
    Back,
    Open(Kind),
    Toast,
}

pub struct PresenceDemo {
    toast: bool,
}

impl PresenceDemo {
    pub(crate) fn new() -> Self {
        Self { toast: false }
    }
}

impl Component for PresenceDemo {
    type Msg = PresenceMsg;

    fn update(&mut self, msg: PresenceMsg, cx: &mut Cx<Self>) {
        match msg {
            PresenceMsg::Back => cx.nav_of::<Root>().pop(),
            PresenceMsg::Open(kind) => cx.nav_of::<Overlay>().push(Shown { kind }),
            PresenceMsg::Toast => {
                self.toast = !self.toast;
                cx.rebuild();
            }
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<PresenceMsg>) -> Element<'a, PresenceMsg> {
        let mut items: Vec<_> = KINDS.iter().map(|(name, id, kind)| list_item(ui, None, name, None, None, PresenceMsg::Open(*kind)).id(id)).collect();
        items.push(list_item(ui, None, "Toast", Some(if self.toast { "Shown" } else { "Hidden" }), None, PresenceMsg::Toast).id("toggle-toast"));
        let note = if self.toast { toast(ui, "Saved to storage").key(1).id("toast").appear(Presence::fade().offset(0, 24)) } else { ui.empty() };
        ui.column([top_bar(ui, Some(PresenceMsg::Back), "Presence", ui.empty()), ui.scroll(ui.column(items)).grow(1), note])
            .padding_each(0, ui.insets().top, 0, 0)
            .background(ui.colors().background)
    }
}

impl Screen for PresenceDemo {}

#[derive(Clone, Copy)]
pub enum ShownMsg {
    Close,
    Open(Kind),
}

/// What a `Kind` puts on the overlay.
pub struct Shown {
    kind: Kind,
}

impl Shown {
    fn content<'a>(&self, ui: &'a Ui<ShownMsg>, title: &'a str, text: &'a str) -> Element<'a, ShownMsg> {
        let theme = ui.theme();
        let b = &theme.widgets.button;
        let next = match self.kind {
            Kind::Sheet => button(ui, "Open a modal", b.primary, ShownMsg::Open(Kind::Modal)).id("next"),
            _ => button(ui, "Open a sheet", b.primary, ShownMsg::Open(Kind::Sheet)).id("next"),
        };
        ui.column([
            ui.text(theme.fonts.title, title).foreground(ui.colors().text),
            ui.text(theme.fonts.body, text).foreground(colors::MUTED).wrap(),
            ui.row([button(ui, "Close", b.secondary, ShownMsg::Close).grow(1).id("close"), next.grow(1)]).gap(theme.widgets.unit.gap),
        ])
        .gap(theme.widgets.unit.padding)
    }

    fn dialog<'a>(&self, ui: &'a Ui<ShownMsg>, content: Element<'a, ShownMsg>, appear: Presence, disappear: Presence) -> Element<'a, ShownMsg> {
        let t = &ui.theme().widgets.modal;
        let (width, height) = ui.screen_size();
        let panel = ui.column([content]).style(t.panel).width(t.width).absolute(Some((width - t.width) / 2), Some(height / 3), None, None);
        ui.column([panel.appear(appear).disappear(disappear)]).absolute(Some(0), Some(0), Some(0), Some(0))
    }
}

impl Component for Shown {
    type Msg = ShownMsg;

    fn update(&mut self, msg: ShownMsg, cx: &mut Cx<Self>) {
        match msg {
            ShownMsg::Close => cx.nav_of::<Overlay>().pop(),
            ShownMsg::Open(kind) => cx.nav_of::<Overlay>().push(Shown { kind }),
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<ShownMsg>) -> Element<'a, ShownMsg> {
        let theme = ui.theme();
        let (c, m) = (ui.colors(), &theme.widgets.unit);
        match self.kind {
            Kind::Modal => modal(ui, self.content(ui, "Modal", "Scales and fades in, fades out. A tap on the scrim closes it.")),
            Kind::Kept => modal(ui, self.content(ui, "Modal", "A tap on the scrim does nothing; only Close closes it.")),
            Kind::ScaleOut => self.dialog(ui, self.content(ui, "At once in", "Shows at once, and scales down as it fades out."), Presence::none(), Presence::fade().scale(0.9)),
            Kind::ShrinkIn => self.dialog(ui, self.content(ui, "Shrink in", "Comes from 1.1 times its size, and goes the same way."), Presence::fade().scale(1.1), Presence::fade().scale(1.1)),
            Kind::Sheet => {
                let sheet = ui
                    .column([self.content(ui, "Bottom sheet", "Comes from the bottom edge and goes back there. Open a modal to replace it at once.")])
                    .padding_each(m.padding, m.padding, m.padding, m.padding + ui.insets().bottom)
                    .radius(m.radius)
                    .background(c.surface)
                    .absolute(Some(0), None, Some(0), Some(0))
                    .id("sheet")
                    .appear(Presence::from_edge(Edge::Bottom));
                ui.column([sheet]).absolute(Some(0), Some(0), Some(0), Some(0))
            }
            Kind::Side => {
                let side = ui
                    .column([self.content(ui, "Side panel", "Comes from the right edge.")])
                    .padding_each(m.padding, m.padding + ui.insets().top, m.padding, m.padding)
                    .width(360)
                    .background(c.surface)
                    .absolute(None, Some(0), Some(0), Some(0))
                    .id("side")
                    .appear(Presence::from_edge(Edge::Right));
                ui.column([side]).absolute(Some(0), Some(0), Some(0), Some(0))
            }
        }
    }
}

impl Screen for Shown {
    fn scrim_tap(&self) -> Option<ShownMsg> {
        match self.kind {
            Kind::Kept => None,
            _ => Some(ShownMsg::Close),
        }
    }
}
