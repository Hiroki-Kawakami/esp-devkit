/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Color32, ColorImage, Sense, TextureHandle, TextureOptions, Vec2};

use crate::device::{Device, Status};

const REFRESH: Duration = Duration::from_millis(33);

pub fn run(device: Arc<Device>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 900.0])
            .with_icon(egui::IconData::default()),
        ..Default::default()
    };
    eframe::run_native("sim_host", options, Box::new(|_| Ok(Box::new(App { device, texture: None, rotation: 0 }))))
}

struct App {
    device: Arc<Device>,
    texture: Option<TextureHandle>,
    /// Quarter turns clockwise.
    rotation: u8,
}

impl App {
    fn controls(&mut self, ui: &mut egui::Ui) {
        let status = self.device.status();
        ui.label(match status {
            Status::Running => "firmware: running".to_string(),
            Status::Exited(Some(code)) => format!("firmware: exited ({code})"),
            Status::Exited(None) => "firmware: killed".to_string(),
            Status::Off => "firmware: off".to_string(),
        });
        ui.horizontal(|ui| {
            if ui.button("Reset").clicked() {
                self.start();
            }
            if status == Status::Running {
                if ui.button("Power off").clicked() {
                    self.device.stop();
                }
            } else if ui.button("Power on").clicked() {
                self.start();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Rotate");
            for (q, name) in ["0°", "90°", "180°", "270°"].into_iter().enumerate() {
                ui.selectable_value(&mut self.rotation, q as u8, name);
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
            for line in self.device.log() {
                ui.monospace(line);
            }
        });
    }

    fn start(&self) {
        if let Err(err) = self.device.start() {
            eprintln!("[sim_host] start: {err}");
        }
    }

    fn panel(&mut self, ui: &mut egui::Ui) {
        let Some(frame) = self.device.frame() else {
            ui.centered_and_justified(|ui| ui.label("no frame"));
            return;
        };
        let (w, h) = (frame.width, frame.height);
        let image = rotate(&frame.rgb, w, h, self.rotation);
        let size = image.size;
        match &mut self.texture {
            Some(t) => t.set(image, TextureOptions::NEAREST),
            None => self.texture = Some(ui.ctx().load_texture("panel", image, TextureOptions::NEAREST)),
        }
        let texture = self.texture.as_ref().unwrap();

        let avail = ui.available_size();
        let scale = (avail.x / size[0] as f32).min(avail.y / size[1] as f32).min(1.0);
        let shown = Vec2::new(size[0] as f32, size[1] as f32) * scale;
        let level = self.device.brightness();
        let tint = Color32::from_gray(if level == 0 { 0 } else { (55 + u16::from(level) * 2).min(255) as u8 });
        let response = ui.add(egui::Image::new((texture.id(), shown)).tint(tint).sense(Sense::click_and_drag()));

        if response.is_pointer_button_down_on() {
            if let Some(pos) = response.interact_pointer_pos() {
                let local = (pos - response.rect.min) / scale;
                let (x, y) = unrotate(local.x, local.y, w, h, self.rotation);
                self.device.touch(0, Some((x, y)));
            }
        } else if response.drag_stopped() || response.clicked() {
            self.device.touch(0, None);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::SidePanel::right("controls").min_width(260.0).show(ctx, |ui| self.controls(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.panel(ui));
        ctx.request_repaint_after(REFRESH);
    }
}

fn rotate(rgb: &[u8], w: usize, h: usize, rotation: u8) -> ColorImage {
    let (ow, oh) = if rotation % 2 == 0 { (w, h) } else { (h, w) };
    let mut pixels = vec![Color32::BLACK; ow * oh];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            let (ox, oy) = match rotation {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                3 => (y, w - 1 - x),
                _ => (x, y),
            };
            pixels[oy * ow + ox] = Color32::from_rgb(rgb[i], rgb[i + 1], rgb[i + 2]);
        }
    }
    ColorImage { size: [ow, oh], pixels }
}

fn unrotate(x: f32, y: f32, w: usize, h: usize, rotation: u8) -> (u16, u16) {
    let (x, y) = (x.max(0.0) as usize, y.max(0.0) as usize);
    let (px, py) = match rotation {
        1 => (y, h.saturating_sub(1 + x)),
        2 => (w.saturating_sub(1 + x), h.saturating_sub(1 + y)),
        3 => (w.saturating_sub(1 + y), x),
        _ => (x, y),
    };
    (px.min(w - 1) as u16, py.min(h - 1) as u16)
}
