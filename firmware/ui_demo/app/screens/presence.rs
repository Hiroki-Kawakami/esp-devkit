/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use devkit_ui::widgets::{button, list_item, modal, modal_actions, modal_message, modal_title, password_field, progress_bar, progress_indicator, text_field, toast, top_bar};
use devkit_ui::{Align, Component, Corners, Cx, Edge, Element, Id, Overlay, Part, Presence, Root, Screen, TextBuffer, TextInput, Ui, id};

use crate::resources::colors;

#[derive(Clone, Copy)]
pub enum Kind {
    Modal,
    Kept,
    ScaleOut,
    ShrinkIn,
    Sheet,
    Side,
    Input,
    Password,
    Progress,
    Busy,
}

const KINDS: [(&str, &str, Kind); 10] = [
    ("Modal", "open-modal", Kind::Modal),
    ("Modal: scrim does nothing", "open-kept", Kind::Kept),
    ("Modal: at once in, scale out", "open-scale-out", Kind::ScaleOut),
    ("Modal: shrink in", "open-shrink-in", Kind::ShrinkIn),
    ("Bottom sheet", "open-sheet", Kind::Sheet),
    ("Side panel", "open-side", Kind::Side),
    ("Modal: text field", "open-input", Kind::Input),
    ("Modal: password", "open-password", Kind::Password),
    ("Modal: progress", "open-progress", Kind::Progress),
    ("Modal: busy", "open-busy", Kind::Busy),
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
            PresenceMsg::Open(kind) => cx.nav_of::<Overlay>().push(Shown::new(kind)),
            PresenceMsg::Toast => {
                self.toast = !self.toast;
                cx.rebuild();
            }
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<PresenceMsg>) -> Element<'a, PresenceMsg> {
        let mut items: Vec<_> = KINDS.iter().map(|(name, id, kind)| list_item(ui, None, name, None, None, PresenceMsg::Open(*kind)).id(Id::named(*id))).collect();
        items.push(list_item(ui, None, "Toast", Some(if self.toast { "Shown" } else { "Hidden" }), None, PresenceMsg::Toast).id(id!("toggle-toast")));
        let note = if self.toast { toast(ui, "Saved to storage").id(id!("toast")).appear(Presence::fade().offset(0, 24)) } else { ui.empty() };
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
    Name(TextInput),
    Secret(TextInput),
    Reveal,
}

/// What a `Kind` puts on the overlay.
pub struct Shown {
    kind: Kind,
    name: TextBuffer,
    secret: TextBuffer,
    shown: bool,
}

impl Shown {
    fn new(kind: Kind) -> Self {
        Self { kind, name: TextBuffer::new(), secret: TextBuffer::new(), shown: false }
    }

    fn content<'a>(&self, ui: &'a Ui<ShownMsg>, title: &'a str, text: &'a str) -> [Element<'a, ShownMsg>; 3] {
        let b = &ui.theme().widgets.button;
        let next = match self.kind {
            Kind::Sheet => button(ui, "Open a modal", b.primary, ShownMsg::Open(Kind::Modal)).id(id!("next")),
            _ => button(ui, "Open a sheet", b.primary, ShownMsg::Open(Kind::Sheet)).id(id!("next")),
        };
        [modal_title(ui, title), modal_message(ui, text), modal_actions(ui, [button(ui, "Close", b.secondary, ShownMsg::Close).id(id!("close")), next])]
    }

    fn dialog<'a>(&self, ui: &'a Ui<ShownMsg>, title: &'a str, text: &'a str, appear: Presence, disappear: Presence) -> Element<'a, ShownMsg> {
        modal(ui, self.content(ui, title, text)).part(Part::Panel, |panel| panel.appear(appear).disappear(disappear))
    }

    fn input<'a>(&'a self, ui: &'a Ui<ShownMsg>) -> Element<'a, ShownMsg> {
        let b = &ui.theme().widgets.button;
        let bad = self.name.as_str().contains('/');
        let error = if bad { modal_message(ui, "A name can't contain /").foreground(colors::DESTRUCTIVE).id(id!("error")) } else { ui.empty() };
        modal(ui, [
            modal_title(ui, "Rename"),
            text_field(ui, &self.name, ShownMsg::Name).placeholder("Name").id(id!("name")),
            error,
            modal_actions(ui, [button(ui, "Cancel", b.secondary, ShownMsg::Close).id(id!("close")), button(ui, "Rename", b.primary, ShownMsg::Close).disabled(bad)]),
        ])
    }
}

impl Shown {
    fn password<'a>(&'a self, ui: &'a Ui<ShownMsg>) -> Element<'a, ShownMsg> {
        let b = &ui.theme().widgets.button;
        modal(ui, [
            modal_title(ui, "Connect"),
            password_field(ui, &self.secret, self.shown, ShownMsg::Secret, ShownMsg::Reveal).placeholder("Password").id(id!("password")),
            modal_actions(ui, [button(ui, "Cancel", b.secondary, ShownMsg::Close).id(id!("close")), button(ui, "Connect", b.primary, ShownMsg::Close)]),
        ])
    }
}

impl Component for Shown {
    type Msg = ShownMsg;

    fn update(&mut self, msg: ShownMsg, cx: &mut Cx<Self>) {
        match msg {
            ShownMsg::Close => cx.nav_of::<Overlay>().pop(),
            ShownMsg::Open(kind) => cx.nav_of::<Overlay>().push(Shown::new(kind)),
            ShownMsg::Name(input) => {
                self.name.apply(input);
                cx.rebuild();
            }
            ShownMsg::Secret(input) => {
                self.secret.apply(input);
                cx.rebuild();
            }
            ShownMsg::Reveal => {
                self.shown = !self.shown;
                cx.rebuild();
            }
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<ShownMsg>) -> Element<'a, ShownMsg> {
        let theme = ui.theme();
        let (c, m) = (ui.colors(), &theme.widgets.unit);
        match self.kind {
            Kind::Modal => modal(ui, self.content(ui, "Modal", "Scales and fades in, fades out. A tap on the scrim closes it.")),
            Kind::Kept => modal(ui, self.content(ui, "Modal", "A tap on the scrim does nothing; only Close closes it.")),
            Kind::ScaleOut => self.dialog(ui, "At once in", "Shows at once, and scales down as it fades out.", Presence::none(), Presence::fade().scale(0.9)),
            Kind::ShrinkIn => self.dialog(ui, "Shrink in", "Comes from 1.1 times its size, and goes the same way.", Presence::fade().scale(1.1), Presence::fade().scale(1.1)),
            Kind::Input => self.input(ui),
            Kind::Password => self.password(ui),
            Kind::Progress => modal(ui, [
                modal_title(ui, "Copying"),
                modal_message(ui, "1/1 big.bin"),
                progress_bar(ui, None),
                modal_actions(ui, [button(ui, "Cancel", theme.widgets.button.secondary, ShownMsg::Close).id(id!("close"))]),
            ]),
            Kind::Busy => modal(ui, [ui.row([progress_indicator(ui, None), modal_message(ui, "Connecting...").grow(1)]).align(Align::Center).gap(m.padding)]),
            Kind::Sheet => {
                let sheet = ui
                    .column(self.content(ui, "Bottom sheet", "Comes from the bottom edge and goes back there. Open a modal to replace it at once."))
                    .gap(m.padding)
                    .padding_each(m.padding, m.padding, m.padding, m.padding + ui.insets().bottom)
                    .radius(m.radius)
                    .corners(Corners::TOP)
                    .background(c.surface)
                    .absolute(Some(0), None, Some(0), Some(0))
                    .id(id!("sheet"))
                    .appear(Presence::from_edge(Edge::Bottom));
                ui.column([sheet]).absolute(Some(0), Some(0), Some(0), Some(0))
            }
            Kind::Side => {
                let side = ui
                    .column(self.content(ui, "Side panel", "Comes from the right edge."))
                    .gap(m.padding)
                    .padding(m.padding)
                    .width(360)
                    .background(c.surface)
                    .absolute(None, Some(0), Some(0), Some(0))
                    .id(id!("side"))
                    .appear(Presence::from_edge(Edge::Right));
                ui.column([side]).absolute(Some(0), Some(0), Some(0), Some(0))
            }
        }
    }
}

impl Screen for Shown {
    fn will_appear(&mut self, cx: &mut Cx<Self>) {
        match self.kind {
            Kind::Input => cx.focus(id!("name")),
            Kind::Password => cx.focus(id!("password")),
            _ => {}
        }
    }

    fn scrim_tap(&self) -> Option<ShownMsg> {
        match self.kind {
            Kind::Kept => None,
            _ => Some(ShownMsg::Close),
        }
    }
}
