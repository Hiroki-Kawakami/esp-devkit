/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use devkit_gfx::{Color, CornerMask, DisplayList, Rect, ShadowMask};
use idf::heap::Caps;

use crate::node::{NONE, NodeContent, NodeId};
use crate::style::Shadow;
use crate::tree::Tree;

/// Corner and shadow masks by size; a UI has a handful of each.
pub(crate) struct Effects {
    corners: Vec<CornerMask>,
    shadows: Vec<(u8, u8, ShadowMask)>,
    caps: Caps,
}

impl Effects {
    pub(crate) fn new(caps: Caps) -> Self {
        Self { corners: Vec::new(), shadows: Vec::new(), caps }
    }

    fn corner(&mut self, radius: u8) -> CornerMask {
        if let Some(corner) = self.corners.iter().find(|c| c.radius() == u16::from(radius)) {
            return *corner;
        }
        let corner = CornerMask::new_in(u16::from(radius), self.caps);
        self.corners.push(corner);
        corner
    }

    fn shadow(&mut self, blur: u8, radius: u8) -> ShadowMask {
        if let Some((_, _, mask)) = self.shadows.iter().find(|(b, r, _)| *b == blur && *r == radius) {
            return *mask;
        }
        let mask = ShadowMask::new_in(u16::from(blur), u16::from(radius), self.caps);
        self.shadows.push((blur, radius, mask));
        mask
    }
}

fn dim(color: Color, opacity: u8) -> Color {
    color.with_opacity(opacity)
}

impl Tree {
    /// `(dx, dy)` is the origin of `id`'s parent content on the screen.
    fn paint_node<'a>(&'a self, id: NodeId, dx: i32, dy: i32, clip: Rect, opacity: u8, effects: &mut Effects, list: &mut DisplayList<'a, Caps>) {
        let node = &self.nodes[id as usize];
        let x = dx + i32::from(node.rect.x) + i32::from(node.translate.0);
        let y = dy + i32::from(node.rect.y) + i32::from(node.translate.1);
        if y >= i32::from(clip.bottom()) || y + i32::from(node.rect.h) + 64 <= i32::from(clip.y) {
            return;
        }
        let (sx, sy) = (x as i16, y as i16);
        let rect = Rect::new(sx, sy, node.rect.w, node.rect.h);
        let style = &node.style;
        let own = node.opacity.or(style.opacity);
        let opacity = own.map_or(opacity, |o| Color::rgba(0, 0, 0, opacity).with_opacity(o).a);

        if let Some(Shadow { y: down, blur, color }) = style.shadow {
            let mask = effects.shadow(blur, style.radius);
            list.shadow(rect.offset(0, i16::from(down)), mask, dim(color, opacity));
        }
        let background = if node.pressed { style.pressed_background.or(style.background) } else { style.background };
        if let Some(color) = background {
            let color = dim(color, opacity);
            if style.radius > 0 {
                list.round_rect(rect, effects.corner(style.radius), color);
            } else {
                list.fill(rect, color);
            }
        }
        if let Some(color) = style.border_top {
            list.fill(Rect::new(sx, sy, rect.w, 1), dim(color, opacity));
        }
        if let Some(color) = style.border_bottom {
            list.fill(Rect::new(sx, rect.bottom() - 1, rect.w, 1), dim(color, opacity));
        }

        let [left, top, _, _] = style.padding;
        let (cx, cy) = (x + i32::from(left), y + i32::from(top));
        match &node.content {
            NodeContent::Text(text) => {
                let color = dim(text.color, opacity);
                list.text(cx as i16, cy as i16, &text.run, color);
            }
            NodeContent::Image { image, tint } => list.image(cx as i16, cy as i16, *image, dim(*tint, opacity)),
            NodeContent::Scroll(scroll) => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                self.paint_children(id, cx, cy - scroll.offset, viewport, opacity, effects, list);
                list.pop_clip();
            }
            NodeContent::List(l) => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                for &(index, row) in &l.rows {
                    let row_y = cy + index as i32 * i32::from(l.row_height) - l.offset;
                    self.paint_node(row, cx, row_y, viewport, opacity, effects, list);
                }
                list.pop_clip();
            }
            _ if style.clip => {
                let viewport = rect.intersect(&clip);
                list.push_clip(viewport);
                self.paint_children(id, x, y, viewport, opacity, effects, list);
                list.pop_clip();
            }
            _ => self.paint_children(id, x, y, clip, opacity, effects, list),
        }
    }

    fn paint_children<'a>(&'a self, id: NodeId, dx: i32, dy: i32, clip: Rect, opacity: u8, effects: &mut Effects, list: &mut DisplayList<'a, Caps>) {
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            self.paint_node(child, dx, dy, clip, opacity, effects, list);
            child = self.nodes[child as usize].next;
        }
    }

    pub(crate) fn paint<'a>(&'a self, effects: &mut Effects, list: &mut DisplayList<'a, Caps>) {
        list.reserve(self.nodes.len());
        self.paint_node(self.root, 0, 0, self.area, 255, effects, list);
    }
}
