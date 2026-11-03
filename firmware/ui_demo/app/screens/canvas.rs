/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::vec::Vec;

use devkit_gfx::{Color, TextOptions, TextRun};
use devkit_ui::widgets::{button, top_bar};
use devkit_ui::{Canvas, Component, Cx, Element, Gesture, Gestures, Phase, Raster, RasterTarget, Rect, Screen, TextInput, Ui, id};

use crate::scene::BODY;

const LINE_HEIGHT: i16 = 32;
const WAVE_PERIOD: i32 = 180;
const SAMPLES: usize = 60;
const CHART_GRID: i16 = 40;

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
    Sample,
    Lines,
    ChartSized(i16, i16),
}

pub struct CanvasDemo {
    lines: Vec<String>,
    runs: Vec<TextRun>,
    columns: i16,
    rows: usize,
    wave: Wave,
    scrub_from: i32,
    samples: Vec<u8>,
    seed: u32,
    chart: Vec<(f32, f32)>,
    chart_size: (i16, i16),
    chart_lines: bool,
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
            samples: Vec::new(),
            seed: 1,
            chart: Vec::new(),
            chart_size: (0, 0),
            chart_lines: true,
        }
    }

    fn sample(&mut self) {
        self.seed = self.seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let last = i32::from(self.samples.last().copied().unwrap_or(50));
        let step = (self.seed >> 16) as i32 % 31 - 15;
        if self.samples.len() == SAMPLES {
            self.samples.remove(0);
        }
        self.samples.push((last + step).clamp(0, 100) as u8);
        self.plot();
    }

    fn plot(&mut self) {
        let (width, height) = (f32::from(self.chart_size.0), f32::from(self.chart_size.1));
        let pitch = width / (SAMPLES - 1) as f32;
        let first = SAMPLES - self.samples.len();
        self.chart = self.samples.iter().enumerate().map(|(i, &v)| ((first + i) as f32 * pitch, height - 4.0 - f32::from(v) * (height - 8.0) / 100.0)).collect();
    }

    fn draw_chart<'a>(&'a self, canvas: &mut Canvas<'a, '_>) {
        let (width, height) = canvas.size();
        let colors = canvas.colors();
        canvas.fill(Rect::new(0, 0, width, height), colors.background);
        for y in (CHART_GRID..height).step_by(CHART_GRID as usize) {
            canvas.fill(Rect::new(0, y, width, 1), colors.divider);
        }
        if self.chart_lines {
            canvas.polyline(&self.chart, 3.0, colors.primary);
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
                cx.redraw(id!("term"));
                return;
            }
            _ => return,
        }
        self.lay_out();
        cx.redraw_rect(id!("term"), self.line_rect(self.runs.len() - 1));
    }

    fn draw_terminal<'a>(&'a self, canvas: &mut Canvas<'a, '_>) {
        let (width, height) = canvas.size();
        canvas.fill(Rect::new(0, 0, width, height), Color::rgb(0x10, 0x14, 0x18));
        for (row, run) in self.runs.iter().enumerate() {
            let rect = Rect::new(0, row as i16 * LINE_HEIGHT, width, LINE_HEIGHT);
            if canvas.needs(rect) {
                canvas.text(8, rect.y, run, Color::rgb(0xE0, 0xE6, 0xEA));
            }
        }
    }

    fn draw_wave<'a>(&'a self, canvas: &mut Canvas<'a, '_>) {
        let (width, height) = canvas.size();
        let colors = canvas.colors();
        canvas.fill(Rect::new(0, 0, width, height), colors.background);
        canvas.raster(Rect::new(0, 0, width, height), &self.wave);
    }
}

impl Component for CanvasDemo {
    type Msg = CanvasMsg;

    fn update(&mut self, msg: CanvasMsg, cx: &mut Cx<Self>) {
        match msg {
            CanvasMsg::Back => cx.nav().pop(),
            CanvasMsg::Redraw => cx.redraw(id!("term")),
            CanvasMsg::Sized(width, height) => {
                self.columns = width;
                self.rows = (height / LINE_HEIGHT).max(1) as usize;
                self.lay_out();
                cx.redraw(id!("term"));
            }
            CanvasMsg::WaveSized(height) => self.wave.height = height,
            CanvasMsg::Key(input) => self.key(input, cx),
            CanvasMsg::Scrub(Gesture::Pan { phase, dx, .. }) => {
                if phase == Phase::Began {
                    self.scrub_from = self.wave.phase;
                }
                self.wave.phase = self.scrub_from - i32::from(dx);
                cx.redraw(id!("wave"));
            }
            CanvasMsg::Scrub(_) => {}
            CanvasMsg::Sample => {
                self.sample();
                cx.redraw(id!("chart"));
            }
            CanvasMsg::Lines => {
                self.chart_lines = !self.chart_lines;
                cx.redraw(id!("chart"));
            }
            CanvasMsg::ChartSized(width, height) => {
                self.chart_size = (width, height);
                while self.samples.len() < SAMPLES {
                    self.sample();
                }
                self.plot();
                cx.redraw(id!("chart"));
            }
        }
    }

    fn view<'a>(&'a self, ui: &'a Ui<CanvasMsg>) -> Element<'a, CanvasMsg> {
        let theme = ui.theme();
        let redraw = button(ui, "Redraw", theme.widgets.button.secondary, CanvasMsg::Redraw).keep_focus().id(id!("term-redraw"));
        let sample = button(ui, "Sample", theme.widgets.button.secondary, CanvasMsg::Sample).keep_focus().id(id!("chart-sample"));
        let lines = button(ui, "Lines", theme.widgets.button.secondary, CanvasMsg::Lines).keep_focus().id(id!("chart-lines"));
        ui.column([
            top_bar(ui, Some(CanvasMsg::Back), "Canvas", ui.row([sample, lines, redraw]).gap(theme.widgets.unit.gap)),
            ui.canvas(Self::draw_terminal).grow(1).id(id!("term")).on_size(CanvasMsg::Sized).on_input(CanvasMsg::Key),
            ui.canvas(Self::draw_chart).height(160).id(id!("chart")).on_size(CanvasMsg::ChartSized),
            ui.canvas(Self::draw_wave).height(160).id(id!("wave")).on_size(|_, height| CanvasMsg::WaveSized(height)).gestures(Gestures::PAN, CanvasMsg::Scrub),
        ])
        .padding_each(0, ui.insets().top, 0, ui.insets().bottom)
        .background(ui.colors().surface)
    }
}

impl Screen for CanvasDemo {}
