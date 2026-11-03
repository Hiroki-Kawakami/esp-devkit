/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use devkit_ui::widgets::{list_item, top_bar};
use devkit_ui::{Component, Cx, Element, Id, Root, Screen, Ui};

use super::canvas::CanvasDemo;
use super::files::FileManager;
use super::gestures::GestureDemo;
use super::log::LogDemo;
use super::presence::PresenceDemo;
use super::settings::Settings;
use super::transitions::TransitionDemo;
use super::video::VideoDemo;

/// The root screen: pages fill the screen when upright, and sit next to the
/// storage list when wide.
const DEMOS: [(&str, &str); 8] = [
    ("Files", "open-files"),
    ("Settings", "open-settings"),
    ("Gestures", "open-gestures"),
    ("Canvas", "open-canvas"),
    ("Video", "open-video"),
    ("Transitions", "open-transitions"),
    ("Presence", "open-presence"),
    ("Log", "open-log"),
];

/// The way into each demo.
pub struct Home;

impl Component for Home {
    type Msg = u8;

    fn update(&mut self, demo: u8, cx: &mut Cx<Self>) {
        let mut nav = cx.nav_of::<Root>();
        match demo {
            0 => nav.push(FileManager),
            1 => nav.push(Settings::new()),
            2 => nav.push(GestureDemo::new()),
            3 => nav.push(CanvasDemo::new()),
            4 => nav.push(VideoDemo::new()),
            5 => nav.push(TransitionDemo::new()),
            6 => nav.push(PresenceDemo::new()),
            _ => nav.push(LogDemo::new()),
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<u8>) -> Element<'a, u8> {
        let items: Vec<_> = DEMOS.iter().enumerate().map(|(i, (name, id))| list_item(ui, None, name, None, None, i as u8).id(Id::named(*id))).collect();
        ui.column([top_bar(ui, None, "ui_demo", ui.empty()), ui.column(items)])
            .padding_each(0, ui.insets().top, 0, 0)
            .background(ui.colors().background)
    }
}

impl Screen for Home {}
