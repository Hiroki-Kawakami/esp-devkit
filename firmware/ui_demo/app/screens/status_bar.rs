/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_ui::{Align, Bars, Component, Cx, Element, Ui, id};

use crate::resources::icons;
use crate::scene::CAPTION;
use super::small_icon;

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
        let color = ui.colors().text;
        let time = ui.format(format_args!("{:02}:{:02}:{:02}", self.seconds / 3600, self.seconds / 60 % 60, self.seconds % 60));
        ui.row([
            ui.text(&CAPTION, time).foreground(color).id(id!("clock")).grow(1),
            ui.icon(small_icon(icons::WIFI)).foreground(color),
            ui.icon(small_icon(icons::BATTERY)).foreground(color),
            ui.text(&CAPTION, "78%").foreground(color),
        ])
        .height(super::STATUS_BAR_HEIGHT)
        .padding_x(24)
        .gap(14)
        .align(Align::Center)
    }
}
