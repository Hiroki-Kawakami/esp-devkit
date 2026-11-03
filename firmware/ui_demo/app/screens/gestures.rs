/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use core::fmt::Write;

use devkit_ui::widgets::top_bar;
use devkit_ui::{Component, Cx, Element, Gesture, Gestures, Phase, Rotations, Screen, ScreenStyle, Ui, id};

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
    pub(crate) fn new() -> Self {
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
        let c = ui.colors();
        let side = (f32::from(BOX_SIZE) * self.scale) as i16;
        let (x, y) = (self.position.0 - side / 2, self.position.1 - side / 2);
        ui.column([
            top_bar(ui, Some(GestureMsg::Back), "Gestures", ui.empty()),
            ui.text(theme.fonts.body, &self.label).padding(theme.widgets.unit.padding).id(id!("gesture-label")),
            ui.column([ui.column([]).size(side, side).radius(theme.widgets.unit.radius).background(c.primary).absolute(Some(x), Some(y), None, None).id(id!("gesture-box"))])
                .grow(1)
                .clip()
                .id(id!("gesture-area"))
                .gestures(Gestures::all(), GestureMsg::Touch)
                .on_tap(GestureMsg::Tap),
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(c.surface)
    }
}

impl Screen for GestureDemo {
    fn style(&self) -> ScreenStyle {
        ScreenStyle { rotations: Rotations::LANDSCAPE, ..ScreenStyle::default() }
    }
}
