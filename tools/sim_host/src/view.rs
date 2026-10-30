/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{
    self, Color32, ColorImage, FontId, Margin, Rect, RichText, Sense, TextStyle, TextureHandle, TextureOptions, Vec2,
    pos2, vec2,
};

use crate::device::{Device, Status};
use crate::frame::DeviceFrame;

const APP_NAME: &str = "ESP Simulator";
const REFRESH: Duration = Duration::from_millis(33);
const CONTROLS_WIDTH: f32 = 320.0;
const PREVIEW_MARGIN: f32 = 24.0;
const ZOOMS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
const BACKGROUND: Color32 = Color32::from_rgb(0x1b, 0x1b, 0x1d);
const DEFAULT_ZOOM: f32 = 0.5;
// Fits the Tab5 outline (974 x 1528) at DEFAULT_ZOOM in either orientation.
const DEFAULT_WINDOW: [f32; 2] = [
    1528.0 * DEFAULT_ZOOM + 2.0 * PREVIEW_MARGIN + CONTROLS_WIDTH + 8.0,
    1528.0 * DEFAULT_ZOOM + 2.0 * PREVIEW_MARGIN + 8.0,
];

pub fn run(device: Arc<Device>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_app_id("esp-simulator")
            .with_inner_size(DEFAULT_WINDOW)
            .with_min_inner_size([640.0, 480.0])
            .with_icon(egui::IconData::default()),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(App::new(device)))
        }),
    )
}

fn style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.item_spacing = vec2(10.0, 10.0);
        s.spacing.button_padding = vec2(14.0, 7.0);
        s.spacing.interact_size.y = 30.0;
        s.text_styles = [
            (TextStyle::Heading, FontId::proportional(18.0)),
            (TextStyle::Body, FontId::proportional(15.0)),
            (TextStyle::Button, FontId::proportional(15.0)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
    });
}

#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    Fit,
    Fixed(f32),
}

struct FrameTexture {
    texture: TextureHandle,
    scale: u32,
    rotation: u8,
}

struct App {
    device: Arc<Device>,
    name: Option<String>,
    frame: Option<DeviceFrame>,
    frame_texture: Option<FrameTexture>,
    panel_texture: Option<TextureHandle>,
    zoom: Zoom,
    /// Quarter turns clockwise.
    rotation: u8,
}

impl App {
    fn new(device: Arc<Device>) -> Self {
        Self {
            device,
            name: None,
            frame: None,
            frame_texture: None,
            panel_texture: None,
            zoom: Zoom::Fixed(DEFAULT_ZOOM),
            rotation: 0,
        }
    }

    fn follow_device(&mut self, ctx: &egui::Context) {
        let name = self.device.name();
        if name == self.name {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(name.clone().unwrap_or_else(|| APP_NAME.into())));
        self.frame = name.as_deref().and_then(DeviceFrame::for_device);
        self.frame_texture = None;
        self.name = name;
    }

    fn start(&self) {
        if let Err(err) = self.device.start() {
            eprintln!("[sim_host] start: {err}");
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        section(ui, "Firmware");
        let status = self.device.status();
        let (color, text) = match status {
            Status::Running => (Color32::from_rgb(0x4c, 0xc2, 0x6a), "Running".to_string()),
            Status::Exited(Some(code)) => (Color32::from_rgb(0xe0, 0x5a, 0x4f), format!("Exited ({code})")),
            Status::Exited(None) => (Color32::from_rgb(0xe0, 0x5a, 0x4f), "Killed".to_string()),
            Status::Off => (Color32::GRAY, "Off".to_string()),
        };
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 5.0, color);
            ui.label(text);
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
        ui.add_space(12.0);

        section(ui, "Preview");
        ui.label("Size");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.spacing_mut().button_padding.x = 9.0;
            ui.selectable_value(&mut self.zoom, Zoom::Fit, "Fit");
            for z in ZOOMS {
                ui.selectable_value(&mut self.zoom, Zoom::Fixed(z), format!("{}%", (z * 100.0) as u32));
            }
        });
        ui.label("Rotation");
        ui.horizontal(|ui| {
            for (q, name) in ["0°", "90°", "180°", "270°"].into_iter().enumerate() {
                ui.selectable_value(&mut self.rotation, q as u8, name);
            }
        });
        ui.add_space(12.0);

        section(ui, "Log");
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(6.0)
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for line in self.device.log() {
                            ui.label(RichText::new(line).monospace());
                        }
                    });
            });
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let image = self.device.frame().map(|f| rotate(rgb_image(&f.rgb, f.width, f.height), self.rotation));
        let panel = image.as_ref().map(|i| i.size).or_else(|| {
            let s = self.frame.as_ref()?.screen().size();
            Some(rotated_size([s.x as usize, s.y as usize], self.rotation))
        });
        let Some(panel) = panel else {
            ui.centered_and_justified(|ui| ui.label(RichText::new("No frame").color(Color32::GRAY)));
            return;
        };
        let outer = match &self.frame {
            Some(f) => {
                let [w, h] = f.size();
                let [w, h] = rotated_size([w as usize, h as usize], self.rotation);
                vec2(w as f32, h as f32)
            }
            None => vec2(panel[0] as f32, panel[1] as f32),
        };
        let avail = ui.available_size();
        let zoom = match self.zoom {
            Zoom::Fit => ((avail.x - 2.0 * PREVIEW_MARGIN) / outer.x).min((avail.y - 2.0 * PREVIEW_MARGIN) / outer.y).max(0.05),
            Zoom::Fixed(z) => z,
        };
        let shown = outer * zoom;

        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let area = Vec2::max(shown + Vec2::splat(2.0 * PREVIEW_MARGIN), avail);
            let (area, _) = ui.allocate_exact_size(area, Sense::hover());
            let rect = Rect::from_center_size(area.center(), shown);

            if let Some(texture) = self.frame_texture(ui.ctx(), zoom) {
                ui.painter().image(texture, rect, full_uv(), Color32::WHITE);
            }
            let screen = match &self.frame {
                Some(frame) => {
                    let [w, h] = frame.size();
                    let s = rotate_rect(frame.screen(), w, h, self.rotation);
                    Rect::from_min_size(rect.min + s.min.to_vec2() * zoom, s.size() * zoom)
                }
                None => rect,
            };

            match image {
                Some(image) => {
                    let texture = match &mut self.panel_texture {
                        Some(t) => {
                            t.set(image, TextureOptions::LINEAR);
                            t.id()
                        }
                        None => {
                            let t = ui.ctx().load_texture("panel", image, TextureOptions::LINEAR);
                            let id = t.id();
                            self.panel_texture = Some(t);
                            id
                        }
                    };
                    ui.painter().image(texture, screen, full_uv(), brightness_tint(self.device.brightness()));
                }
                None => {
                    ui.painter().rect_filled(screen, 0.0, Color32::BLACK);
                }
            }

            let response = ui.interact(screen, ui.id().with("screen"), Sense::click_and_drag());
            if response.is_pointer_button_down_on() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let local = (pos - screen.min) / zoom;
                    let (x, y) = unrotate(local.x, local.y, panel[0], panel[1], self.rotation);
                    self.device.touch(0, Some((x, y)));
                }
            } else if response.drag_stopped() || response.clicked() {
                self.device.touch(0, None);
            }
        });
    }

    fn frame_texture(&mut self, ctx: &egui::Context, zoom: f32) -> Option<egui::TextureId> {
        let frame = self.frame.as_ref()?;
        let scale = zoom * ctx.pixels_per_point();
        let key = (scale * 1000.0).round() as u32;
        let stale = self.frame_texture.as_ref().is_none_or(|t| t.scale != key || t.rotation != self.rotation);
        if stale {
            let image = rotate(frame.render(scale)?, self.rotation);
            let texture = ctx.load_texture("device-frame", image, TextureOptions::LINEAR);
            self.frame_texture = Some(FrameTexture { texture, scale: key, rotation: self.rotation });
        }
        self.frame_texture.as_ref().map(|t| t.texture.id())
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.follow_device(ctx);
        egui::SidePanel::right("controls")
            .resizable(false)
            .exact_width(CONTROLS_WIDTH)
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(Margin::same(18)))
            .show(ctx, |ui| self.controls(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BACKGROUND))
            .show(ctx, |ui| self.preview(ui));
        ctx.request_repaint_after(REFRESH);
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).heading().strong());
    ui.separator();
}

fn full_uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

fn brightness_tint(level: u8) -> Color32 {
    Color32::from_gray(if level == 0 { 0 } else { (55 + u16::from(level) * 2).min(255) as u8 })
}

fn rgb_image(rgb: &[u8], w: usize, h: usize) -> ColorImage {
    let pixels = rgb.chunks_exact(3).map(|p| Color32::from_rgb(p[0], p[1], p[2])).collect();
    ColorImage { size: [w, h], pixels }
}

fn rotated_size([w, h]: [usize; 2], rotation: u8) -> [usize; 2] {
    if rotation % 2 == 0 { [w, h] } else { [h, w] }
}

fn rotate(image: ColorImage, rotation: u8) -> ColorImage {
    if rotation % 4 == 0 {
        return image;
    }
    let [w, h] = image.size;
    let [ow, oh] = rotated_size(image.size, rotation);
    let mut pixels = vec![Color32::TRANSPARENT; ow * oh];
    for y in 0..h {
        for x in 0..w {
            let (ox, oy) = match rotation {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            pixels[oy * ow + ox] = image.pixels[y * w + x];
        }
    }
    ColorImage { size: [ow, oh], pixels }
}

/// `r` inside a `w` x `h` picture, after the picture turns like `rotate`.
fn rotate_rect(r: Rect, w: f32, h: f32, rotation: u8) -> Rect {
    let (x, y, rw, rh) = (r.min.x, r.min.y, r.width(), r.height());
    let (nx, ny, nw, nh) = match rotation % 4 {
        1 => (h - (y + rh), x, rh, rw),
        2 => (w - (x + rw), h - (y + rh), rw, rh),
        3 => (y, w - (x + rw), rh, rw),
        _ => (x, y, rw, rh),
    };
    Rect::from_min_size(pos2(nx, ny), vec2(nw, nh))
}

/// Shown coordinates back to panel pixels; `w` x `h` is the shown size.
fn unrotate(x: f32, y: f32, w: usize, h: usize, rotation: u8) -> (u16, u16) {
    let [pw, ph] = rotated_size([w, h], rotation);
    let (x, y) = (x.max(0.0) as usize, y.max(0.0) as usize);
    let (px, py) = match rotation % 4 {
        1 => (y, w.saturating_sub(1 + x)),
        2 => (pw.saturating_sub(1 + x), ph.saturating_sub(1 + y)),
        3 => (pw.saturating_sub(1 + y), x),
        _ => (x, y),
    };
    (px.min(pw - 1) as u16, py.min(ph - 1) as u16)
}

#[cfg(test)]
mod tests {
    use super::{rotate_rect, unrotate};
    use eframe::egui::{Rect, pos2, vec2};

    #[test]
    fn unrotate_maps_corners_back_to_the_panel() {
        let (pw, ph) = (720usize, 1280usize);
        for rotation in 0..4u8 {
            let [w, h] = super::rotated_size([pw, ph], rotation);
            let origin = match rotation {
                1 => (w - 1, 0),
                2 => (w - 1, h - 1),
                3 => (0, h - 1),
                _ => (0, 0),
            };
            assert_eq!(unrotate(origin.0 as f32, origin.1 as f32, w, h, rotation), (0, 0), "rotation {rotation}");
        }
    }

    #[test]
    fn rotate_rect_keeps_the_screen_inside() {
        let r = Rect::from_min_size(pos2(128.0, 128.0), vec2(720.0, 1280.0));
        assert_eq!(rotate_rect(r, 974.0, 1528.0, 1), Rect::from_min_size(pos2(120.0, 128.0), vec2(1280.0, 720.0)));
        assert_eq!(rotate_rect(r, 974.0, 1528.0, 2), Rect::from_min_size(pos2(126.0, 120.0), vec2(720.0, 1280.0)));
    }
}
