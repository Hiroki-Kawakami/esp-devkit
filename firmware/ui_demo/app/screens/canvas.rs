/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::vec::Vec;

use devkit_gfx::{Color, TextOptions, TextRun};
use devkit_ui::widgets::{button, top_bar};
use devkit_ui::{Canvas, CanvasId, Component, Cx, Element, Gesture, Gestures, Phase, Raster, RasterTarget, Rect, Screen, TextInput, Ui};

use crate::scene::BODY;

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
    pub(crate) fn new() -> Self {
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
        let redraw = button(ui, "Redraw", theme.widgets.button.secondary, CanvasMsg::Redraw).keep_focus().id("term-redraw");
        ui.column([
            top_bar(ui, Some(CanvasMsg::Back), "Canvas", redraw),
            ui.canvas(TERMINAL).grow(1).id("term").on_size(CanvasMsg::Sized).on_input(CanvasMsg::Key),
            ui.canvas(WAVE).height(160).id("wave").on_size(|_, height| CanvasMsg::WaveSized(height)).gestures(Gestures::PAN, CanvasMsg::Scrub),
        ])
        .padding_each(0, ui.insets().top, 0, ui.insets().bottom)
        .background(ui.colors().surface)
    }

    fn draw<'a>(&'a self, id: CanvasId, canvas: &mut Canvas<'a, '_>) {
        let (width, height) = canvas.size();
        let colors = canvas.colors();
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
