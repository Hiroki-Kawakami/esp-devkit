/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::{Align as TextAlign, Rect};

use crate::node::{NONE, NodeContent, NodeId};
use crate::style::{Align, Dimension, Direction, Justify, Style};
use crate::tree::Tree;

/// No limit on an axis.
pub(crate) const UNBOUNDED: i16 = i16::MAX;

fn inner(avail: i16, before: i16, after: i16) -> i16 {
    if avail == UNBOUNDED { UNBOUNDED } else { (avail - before - after).max(0) }
}

fn fixed(dimension: Dimension, offered: i16) -> Option<i16> {
    match dimension {
        Dimension::Px(px) => Some(px),
        Dimension::Fraction(n, d) if offered != UNBOUNDED => Some((i32::from(offered) * i32::from(n) / i32::from(d)) as i16),
        _ => None,
    }
}

fn clamp(size: i16, min: i16, max: i16) -> i16 {
    let size = if max > 0 { size.min(max) } else { size };
    size.max(min)
}

fn offer(limit: i16, max: i16) -> i16 {
    if max > 0 { limit.min(max) } else { limit }
}

impl Tree {
    /// The next child after `child` that is in the flow.
    fn flow(&self, mut child: NodeId) -> NodeId {
        while child != NONE && self.nodes[child as usize].style.absolute.is_some() {
            child = self.nodes[child as usize].next;
        }
        child
    }

    fn first_flow(&self, id: NodeId) -> NodeId {
        self.flow(self.nodes[id as usize].first_child)
    }

    fn next_flow(&self, id: NodeId) -> NodeId {
        self.flow(self.nodes[id as usize].next)
    }

    /// The size `id` wants within the limits.
    pub(crate) fn measure(&mut self, id: NodeId, avail_w: i16, avail_h: i16) -> (i16, i16) {
        let node = &self.nodes[id as usize];
        if let Some((w, h, mw, mh)) = node.measured
            && w == avail_w
            && h == avail_h
        {
            return (mw, mh);
        }
        let limits = (avail_w, avail_h);
        let style = node.style;
        let fixed_w = fixed(style.width, avail_w);
        let fixed_h = fixed(style.height, avail_h);
        let avail_w = offer(fixed_w.unwrap_or(avail_w), style.max_width);
        let avail_h = offer(fixed_h.unwrap_or(avail_h), style.max_height);
        let [left, top, right, bottom] = style.padding;
        let content_w = inner(avail_w, left, right);
        let content_h = inner(avail_h, top, bottom);
        let (w, h) = match &self.nodes[id as usize].content {
            NodeContent::Text(_) => self.measure_text(id, content_w),
            NodeContent::Image { image, .. } => (image.width() as i16, image.height() as i16),
            NodeContent::Scroll(_) | NodeContent::List(_) | NodeContent::Navigator(_) => (content_w.min(0), content_h.min(0)),
            NodeContent::Child(_) => {
                let child = self.nodes[id as usize].first_child;
                if child == NONE { (0, 0) } else { self.measure(child, content_w, content_h) }
            }
            NodeContent::Box => self.measure_box(id, &style, content_w, content_h),
        };
        let mut width = clamp(fixed_w.unwrap_or(w + left + right), style.min_width, style.max_width);
        let mut height = clamp(fixed_h.unwrap_or(h + top + bottom), style.min_height, style.max_height);
        if let Some((a, b)) = style.aspect {
            if fixed_h.is_none() && avail_w != UNBOUNDED {
                width = avail_w;
                height = (i32::from(avail_w) * i32::from(b) / i32::from(a)) as i16;
            } else if fixed_w.is_none() {
                width = (i32::from(height) * i32::from(a) / i32::from(b)) as i16;
            }
        }
        self.nodes[id as usize].measured = Some((limits.0, limits.1, width, height));
        (width, height)
    }

    fn measure_text(&mut self, id: NodeId, width: i16) -> (i16, i16) {
        let NodeContent::Text(text) = &mut self.nodes[id as usize].content else { return (0, 0) };
        // Narrower than laid out but still fitting: the same glyphs.
        let fits = text.options.align == TextAlign::Start
            && text.laid_out.is_some_and(|laid_out| width <= laid_out && text.run.width() <= width);
        if text.laid_out != Some(width) && !fits {
            let mut options = text.options;
            options.max_width = (width != UNBOUNDED).then_some(width);
            text.run.layout(text.font, unsafe { core::str::from_utf8_unchecked(&text.string) }, &options);
            text.laid_out = Some(width);
        }
        (text.run.width(), text.run.height())
    }

    fn measure_box(&mut self, id: NodeId, style: &Style, content_w: i16, content_h: i16) -> (i16, i16) {
        match style.direction {
            Direction::Column => self.solve_axis(id, style, content_h, content_w, true),
            Direction::Row => self.solve_axis(id, style, content_w, content_h, false),
            Direction::Grid(columns) => {
                let columns = i16::from(columns.max(1));
                let cell = content_w.saturating_sub((columns - 1) * style.gap_x) / columns;
                let (mut h, mut row, mut index) = (0, 0, 0);
                let mut child = self.first_flow(id);
                while child != NONE {
                    if index > 0 && index % columns == 0 {
                        h += row + style.gap_y;
                        row = 0;
                    }
                    row = row.max(self.measure(child, cell, UNBOUNDED).1);
                    index += 1;
                    child = self.next_flow(child);
                }
                (content_w, h + row)
            }
        }
    }

    /// Gives `id` its rectangle, relative to the parent's content.
    pub(crate) fn arrange(&mut self, id: NodeId, rect: Rect) {
        self.nodes[id as usize].rect = rect;
        let style = self.nodes[id as usize].style;
        let [left, top, right, bottom] = style.padding;
        let content = Rect::new(left, top, inner(rect.w, left, right), inner(rect.h, top, bottom));
        match &self.nodes[id as usize].content {
            NodeContent::Box => self.arrange_box(id, &style, content),
            NodeContent::Child(_) => {
                let child = self.nodes[id as usize].first_child;
                if child != NONE {
                    self.arrange(child, content);
                }
            }
            NodeContent::Navigator(_) => {
                let mut child = self.nodes[id as usize].first_child;
                while child != NONE {
                    self.arrange(child, content);
                    child = self.nodes[child as usize].next;
                }
            }
            NodeContent::Scroll(_) => {
                let child = self.nodes[id as usize].first_child;
                if child != NONE {
                    let (_, h) = self.measure(child, content.w, UNBOUNDED);
                    self.arrange(child, Rect::new(0, 0, content.w, h));
                    if let NodeContent::Scroll(scroll) = &mut self.nodes[id as usize].content {
                        scroll.content = i32::from(h);
                        scroll.offset = scroll.offset.min((i32::from(h) - i32::from(content.h)).max(0));
                    }
                }
            }
            NodeContent::List(list) => {
                let (row_height, rows) = (list.row_height, list.rows.clone());
                for (_, row) in rows {
                    self.arrange(row, Rect::new(0, 0, content.w, row_height));
                }
            }
            NodeContent::Text(_) => {
                let _ = self.measure_text(id, content.w);
            }
            NodeContent::Image { .. } => {}
        }
        self.arrange_absolute(id, rect);
    }

    fn arrange_absolute(&mut self, id: NodeId, parent: Rect) {
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            let node = &self.nodes[child as usize];
            let next = node.next;
            if let Some(inset) = node.style.absolute {
                let avail_w = match (inset.left, inset.right) {
                    (Some(l), Some(r)) => parent.w - l - r,
                    _ => parent.w,
                };
                let avail_h = match (inset.top, inset.bottom) {
                    (Some(t), Some(b)) => parent.h - t - b,
                    _ => parent.h,
                };
                let (mw, mh) = self.measure(child, avail_w, avail_h);
                let w = if inset.left.is_some() && inset.right.is_some() { avail_w } else { mw };
                let h = if inset.top.is_some() && inset.bottom.is_some() { avail_h } else { mh };
                let x = inset.left.or(inset.right.map(|r| parent.w - r - w)).unwrap_or(0);
                let y = inset.top.or(inset.bottom.map(|b| parent.h - b - h)).unwrap_or(0);
                self.arrange(child, Rect::new(x, y, w, h));
            }
            child = next;
        }
    }

    fn arrange_box(&mut self, id: NodeId, style: &Style, content: Rect) {
        match style.direction {
            Direction::Column => self.arrange_axis(id, style, content, true),
            Direction::Row => self.arrange_axis(id, style, content, false),
            Direction::Grid(columns) => {
                let columns = i16::from(columns.max(1));
                let cell = (content.w - (columns - 1) * style.gap_x) / columns;
                let mut y = content.y;
                let mut start = self.first_flow(id);
                while start != NONE {
                    let (mut row, mut child) = (0, start);
                    for _ in 0..columns {
                        if child == NONE {
                            break;
                        }
                        row = row.max(self.measure(child, cell, UNBOUNDED).1);
                        child = self.next_flow(child);
                    }
                    let mut x = content.x;
                    child = start;
                    for _ in 0..columns {
                        if child == NONE {
                            break;
                        }
                        self.arrange(child, Rect::new(x, y, cell, row));
                        x += cell + style.gap_x;
                        child = self.next_flow(child);
                    }
                    start = child;
                    y += row + style.gap_y;
                }
            }
        }
    }

    /// Sizes the flow children along the main axis, as CSS flex does: each asks
    /// for what it wants; overflow comes out of the shrinking ones and free
    /// space goes to the growing ones, and those are measured again, which is
    /// where wrapped text gets its height. Returns the size of the flow, in
    /// the box's (width, height).
    fn solve_axis(&mut self, id: NodeId, style: &Style, main: i16, cross: i16, vertical: bool) -> (i16, i16) {
        let gap = if vertical { style.gap_y } else { style.gap_x };
        let (mut used, mut count, mut grow, mut shrink_total) = (0i32, 0i32, 0u32, 0i64);
        let mut child = self.first_flow(id);
        while child != NONE {
            let node_style = self.nodes[child as usize].style;
            let mut size = 0;
            if node_style.grow == 0 {
                let (w, h) = if vertical { self.measure(child, cross, main_offer(node_style.height, main)) } else { self.measure(child, main_offer(node_style.width, main), cross) };
                size = if vertical { h } else { w };
                shrink_total += i64::from(size) * i64::from(node_style.shrink);
            }
            grow += u32::from(node_style.grow);
            used += i32::from(size);
            count += 1;
            let node = &mut self.nodes[child as usize];
            node.flow_size = size;
            node.flow_resized = false;
            child = self.next_flow(child);
        }
        let gaps = i32::from(gap) * (count - 1).max(0);
        let bounded = main != UNBOUNDED;
        let overflow = if bounded { used + gaps - i32::from(main) } else { 0 };
        let free = if bounded { (i32::from(main) - used - gaps).max(0) } else { 0 };
        let (mut total, mut across) = (gaps, 0i16);
        child = self.first_flow(id);
        while child != NONE {
            let node = &self.nodes[child as usize];
            let (node_style, mut size) = (node.style, node.flow_size);
            let mut resized = node_style.grow > 0 && bounded;
            if resized {
                size = (i64::from(free) * i64::from(node_style.grow) / i64::from(grow)) as i16;
            } else if overflow > 0 && node_style.shrink > 0 && shrink_total > 0 {
                let weight = i64::from(size) * i64::from(node_style.shrink);
                let cut = ((i64::from(overflow) * weight + shrink_total - 1) / shrink_total) as i16;
                size = (size - cut).max(if vertical { node_style.min_height } else { node_style.min_width }).max(0);
                resized = true;
            }
            let (w, h) = match (resized, vertical) {
                (true, true) => self.measure(child, cross, size),
                (true, false) => self.measure(child, size, cross),
                (false, true) => self.measure(child, cross, main_offer(node_style.height, main)),
                (false, false) => self.measure(child, main_offer(node_style.width, main), cross),
            };
            if !resized && node_style.grow > 0 {
                size = if vertical { h } else { w };
            }
            let node = &mut self.nodes[child as usize];
            node.flow_size = size;
            node.flow_resized = resized;
            total += i32::from(size);
            across = across.max(if vertical { w } else { h });
            child = self.next_flow(child);
        }
        let total = total.min(i32::from(i16::MAX)) as i16;
        if vertical { (across, total) } else { (total, across) }
    }

    fn arrange_axis(&mut self, id: NodeId, style: &Style, content: Rect, vertical: bool) {
        let (main, cross) = if vertical { (content.h, content.w) } else { (content.w, content.h) };
        let gap = if vertical { style.gap_y } else { style.gap_x };
        let (solved_w, solved_h) = self.solve_axis(id, style, main, cross, vertical);
        let leftover = (main - if vertical { solved_h } else { solved_w }).max(0);
        let mut count = 0;
        let mut child = self.first_flow(id);
        while child != NONE {
            count += 1;
            child = self.next_flow(child);
        }
        let (mut position, spacing) = match style.justify {
            Justify::Start => (0, 0),
            Justify::Center => (leftover / 2, 0),
            Justify::End => (leftover, 0),
            Justify::SpaceBetween if count > 1 => (0, leftover / (count - 1)),
            Justify::SpaceBetween => (0, 0),
        };
        child = self.first_flow(id);
        while child != NONE {
            let node = &self.nodes[child as usize];
            let (node_style, size, resized) = (node.style, node.flow_size, node.flow_resized);
            let (w, h) = match (resized, vertical) {
                (true, true) => self.measure(child, cross, size),
                (true, false) => self.measure(child, size, cross),
                (false, true) => self.measure(child, cross, main_offer(node_style.height, main)),
                (false, false) => self.measure(child, main_offer(node_style.width, main), cross),
            };
            let wanted = if vertical { w } else { h };
            let align = node_style.align_self.unwrap_or(style.align);
            let cross_style = if vertical { node_style.width } else { node_style.height };
            let cross_max = if vertical { node_style.max_width } else { node_style.max_height };
            let (offset, length) = match align {
                Align::Stretch if cross_style == Dimension::Auto => (0, clamp(cross, 0, cross_max)),
                Align::Stretch | Align::Start => (0, wanted),
                Align::Center => ((cross - wanted) / 2, wanted),
                Align::End => (cross - wanted, wanted),
            };
            let rect = if vertical {
                Rect::new(content.x + offset, content.y + position, length, size)
            } else {
                Rect::new(content.x + position, content.y + offset, size, length)
            };
            self.arrange(child, rect);
            position += size + gap + spacing;
            child = self.next_flow(child);
        }
    }
}

/// What a child is offered along the main axis: nothing limits what it wants,
/// but a share of the parent needs the parent's size.
fn main_offer(dimension: Dimension, main: i16) -> i16 {
    if matches!(dimension, Dimension::Fraction(..)) { main } else { UNBOUNDED }
}
