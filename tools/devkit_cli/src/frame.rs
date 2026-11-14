/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use eframe::egui::{ColorImage, Rect, pos2, vec2};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

/// A device outline drawn around the panel, measured in panel pixels.
pub struct DeviceFrame {
    tree: Tree,
    size: [f32; 2],
    /// Where the panel goes.
    screen: Rect,
}

// `size` and `screen` are in viewBox units, which the SVGs keep at panel pixels;
// usvg reports the width/height attributes instead.
struct Known {
    name: &'static str,
    svg: &'static str,
    size: [f32; 2],
    screen: [f32; 4],
}

const KNOWN: &[Known] = &[Known {
    name: "M5Stack Tab5",
    svg: include_str!("../assets/m5stack_tab5.svg"),
    size: [974.0, 1528.0],
    screen: [128.0, 128.0, 720.0, 1280.0],
}];

impl DeviceFrame {
    pub fn for_device(name: &str) -> Option<Self> {
        let known = KNOWN.iter().find(|k| k.name == name)?;
        let tree = Tree::from_str(known.svg, &Options::default())
            .inspect_err(|err| eprintln!("[devkit] frame for {name}: {err}"))
            .ok()?;
        let [x, y, w, h] = known.screen;
        Some(Self {
            tree,
            size: known.size,
            screen: Rect::from_min_size(pos2(x, y), vec2(w, h)),
        })
    }

    pub fn size(&self) -> [f32; 2] {
        self.size
    }

    pub fn screen(&self) -> Rect {
        self.screen
    }

    /// Rasterized at `scale` pixels per panel pixel.
    pub fn render(&self, scale: f32) -> Option<ColorImage> {
        let [w, h] = self.size;
        let mut pixmap = Pixmap::new((w * scale).ceil() as u32, (h * scale).ceil() as u32)?;
        let s = scale * w / self.tree.size().width();
        resvg::render(
            &self.tree,
            Transform::from_scale(s, s),
            &mut pixmap.as_mut(),
        );
        let size = [pixmap.width() as usize, pixmap.height() as usize];
        Some(ColorImage::from_rgba_premultiplied(size, pixmap.data()))
    }
}
