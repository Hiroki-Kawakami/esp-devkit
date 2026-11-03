/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_ui::widgets::{button, list_item, top_bar};
use devkit_ui::{Component, Cx, Element, Root, Screen, ScrollEdge, ScrollTarget, Ui, id};

#[derive(Clone, Copy)]
pub enum LogMsg {
    Back,
    Add,
    Top,
    End,
    Row,
}

pub struct LogDemo {
    lines: usize,
}

impl LogDemo {
    pub(crate) fn new() -> Self {
        Self { lines: 30 }
    }

    fn row<'a>(&'a self, index: usize, ui: &'a Ui<LogMsg>) -> Element<'a, LogMsg> {
        list_item(ui, None, ui.format(format_args!("Line {index}")), None, None, LogMsg::Row)
    }
}

impl Component for LogDemo {
    type Msg = LogMsg;

    fn update(&mut self, msg: LogMsg, cx: &mut Cx<Self>) {
        match msg {
            LogMsg::Back => cx.nav_of::<Root>().pop(),
            LogMsg::Add => {
                self.lines += 5;
                cx.rebuild();
            }
            LogMsg::Top => cx.scroll_to(id!("log"), ScrollTarget::Start, true),
            LogMsg::End => cx.scroll_to(id!("log"), ScrollTarget::End, true),
            LogMsg::Row => cx.scroll_to(id!("log"), ScrollTarget::Index(20), true),
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<LogMsg>) -> Element<'a, LogMsg> {
        let theme = ui.theme();
        let b = &theme.widgets.button;
        let actions = ui.row([
            button(ui, "Add", b.secondary, LogMsg::Add).id(id!("log-add")),
            button(ui, "Top", b.secondary, LogMsg::Top).id(id!("log-top")),
            button(ui, "End", b.secondary, LogMsg::End).id(id!("log-end")),
            button(ui, "20", b.secondary, LogMsg::Row).id(id!("log-row")),
        ])
        .gap(theme.widgets.unit.gap);
        ui.column([
            top_bar(ui, Some(LogMsg::Back), "Log", actions),
            ui.list(self.lines, theme.widgets.list.row_height, Self::row).scroll_anchor(ScrollEdge::End).grow(1).id(id!("log")),
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(ui.colors().surface)
    }
}

impl Screen for LogDemo {}
