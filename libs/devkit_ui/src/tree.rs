/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::ptr::NonNull;
use core::slice;

use devkit_gfx::Rect;
use idf::heap::Caps;

use crate::bump::Bump;
use crate::component::{AnyHost, Component, EventMap, Host};
use crate::element::{Content, Element};
use crate::erased::Erased;
use crate::node::{ListNode, NONE, Node, NodeContent, NodeId, ScrollNode, TextNode};
use crate::style::{Dimension, Role};

const OVERSCAN_ROWS: i32 = 1;
const TAP_SLOP: i32 = 12;
const FLING_MIN: f32 = 0.05;
/// Time constant of the speed average: older movement fades with this.
const VELOCITY_TAU_MS: f32 = 40.0;
/// A finger still for this long before lifting throws nothing.
const FLING_STALE_MS: u32 = 80;
/// Share of the speed lost per millisecond.
const FLING_DECAY: f32 = 0.0025;
const MAX_FLING_STEP_MS: f32 = 32.0;

struct Slot {
    host: Option<Box<dyn AnyHost>>,
    node: NodeId,
    events: Option<EventMap>,
}

#[derive(Clone, Copy)]
struct Contact {
    start: (i16, i16),
    last: (i16, i16),
    /// When the contact last moved; a sample at the same place is not news.
    last_ms: u32,
    tap: NodeId,
    scroller: NodeId,
    dragging: bool,
    velocity: f32,
}

#[derive(Clone, Copy)]
struct Hit {
    tap: NodeId,
    scroller: NodeId,
}

/// Everything below the display: components, nodes, layout and gestures.
pub(crate) struct Tree {
    pub(crate) nodes: Vec<Node, Caps>,
    free: Vec<NodeId>,
    pub(crate) root: NodeId,
    pub(crate) area: Rect,
    slots: Vec<Slot>,
    lists: Vec<NodeId>,
    bump: Box<Bump>,
    caps: Caps,
    depth: u32,
    queue: VecDeque<(u16, Erased)>,
    dirty: Vec<u16>,
    layout_dirty: bool,
    damage: Vec<Rect>,
    contact: Option<Contact>,
    fling: Option<(NodeId, f32)>,
}

impl Tree {
    pub(crate) fn new<C: Component>(root: C, area: Rect, caps: Caps) -> Self {
        let mut tree = Self {
            nodes: Vec::new_in(caps),
            free: Vec::new(),
            root: NONE,
            area,
            slots: Vec::new(),
            lists: Vec::new(),
            bump: Box::new(Bump::new(caps)),
            caps,
            depth: 0,
            queue: VecDeque::new(),
            dirty: Vec::new(),
            layout_dirty: true,
            damage: Vec::new(),
            contact: None,
            fling: None,
        };
        tree.root = tree.alloc_node(Node::new(NodeContent::Child(0), 0));
        tree.slots.push(Slot { host: Some(Box::new(Host::<C> { component: root, props: None })), node: tree.root, events: None });
        tree.dirty.push(0);
        tree
    }

    pub(crate) fn bump(&self) -> NonNull<Bump> {
        NonNull::from(&*self.bump)
    }

    fn alloc_node(&mut self, node: Node) -> NodeId {
        match self.free.pop() {
            Some(id) => {
                self.nodes[id as usize] = node;
                id
            }
            None => {
                self.nodes.push(node);
                (self.nodes.len() - 1) as NodeId
            }
        }
    }

    fn free_subtree(&mut self, id: NodeId) {
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            let next = self.nodes[child as usize].next;
            self.free_subtree(child);
            child = next;
        }
        match &self.nodes[id as usize].content {
            NodeContent::List(list) => {
                let rows = list.rows.clone();
                for (_, row) in rows {
                    self.free_subtree(row);
                }
                self.lists.retain(|l| *l != id);
            }
            NodeContent::Child(slot) => {
                let slot = usize::from(*slot);
                self.slots[slot].host = None;
                self.slots[slot].events = None;
                self.slots[slot].node = NONE;
            }
            _ => {}
        }
        let node = &mut self.nodes[id as usize];
        node.content = NodeContent::Box;
        node.first_child = NONE;
        self.free.push(id);
        if let Some(contact) = &mut self.contact {
            if contact.tap == id {
                contact.tap = NONE;
            }
            if contact.scroller == id {
                contact.scroller = NONE;
            }
        }
        if self.fling.is_some_and(|(node, _)| node == id) {
            self.fling = None;
        }
    }

    fn build_component(&mut self, slot: u16) {
        let index = usize::from(slot);
        let node = self.slots[index].node;
        let Some(host) = self.slots[index].host.take() else { return };
        self.depth += 1;
        host.build(self, slot, node);
        self.depth -= 1;
        self.slots[index].host = Some(host);
        self.dirty.retain(|d| *d != slot);
        if self.depth == 0 {
            self.bump.reset();
        }
    }

    /// Makes `element` the only child of `parent`.
    pub(crate) fn reconcile<M: Copy>(&mut self, owner: u16, parent: NodeId, element: &Element<'_, M>) {
        self.reconcile_children(owner, parent, slice::from_ref(element));
    }

    fn matches<M: Copy>(&mut self, id: NodeId, element: &Element<'_, M>) -> bool {
        let node = &self.nodes[id as usize];
        if node.key != element.key {
            return false;
        }
        match (&node.content, &element.content) {
            (NodeContent::Child(slot), Content::Child(spec)) => {
                let host = &mut self.slots[usize::from(*slot)].host;
                host.as_mut().is_some_and(|h| (*h.as_any()).type_id() == spec.host_type())
            }
            (content, other) => content.same_kind(other),
        }
    }

    /// The common case of the same children in the same places.
    fn reconcile_in_place<M: Copy>(&mut self, owner: u16, parent: NodeId, elements: &[Element<'_, M>]) -> bool {
        let mut child = self.nodes[parent as usize].first_child;
        for element in elements.iter().filter(|e| !e.is_empty()) {
            if child == NONE || element.key.is_some() || !self.matches(child, element) {
                return false;
            }
            child = self.nodes[child as usize].next;
        }
        if child != NONE {
            return false;
        }
        child = self.nodes[parent as usize].first_child;
        for element in elements.iter().filter(|e| !e.is_empty()) {
            self.update_node(owner, child, element);
            child = self.nodes[child as usize].next;
        }
        true
    }

    fn reconcile_children<M: Copy>(&mut self, owner: u16, parent: NodeId, elements: &[Element<'_, M>]) {
        if self.reconcile_in_place(owner, parent, elements) {
            return;
        }
        let mut old = Vec::new();
        let mut child = self.nodes[parent as usize].first_child;
        while child != NONE {
            old.push(child);
            child = self.nodes[child as usize].next;
        }
        let was = old.clone();
        let mut cursor = 0;
        let mut new = Vec::with_capacity(elements.len());
        for element in elements.iter().filter(|e| !e.is_empty()) {
            let found = if element.key.is_some() {
                old.iter().position(|&id| id != NONE && self.nodes[id as usize].key == element.key)
            } else {
                while cursor < old.len() && (old[cursor] == NONE || self.nodes[old[cursor] as usize].key.is_some()) {
                    cursor += 1;
                }
                (cursor < old.len()).then_some(cursor)
            };
            let matched = found.filter(|&i| self.matches(old[i], element));
            let id = match matched {
                Some(i) => {
                    let id = old[i];
                    old[i] = NONE;
                    self.update_node(owner, id, element);
                    id
                }
                None => self.create_node(owner, element),
            };
            self.nodes[id as usize].parent = parent;
            new.push(id);
        }
        for id in old.into_iter().filter(|id| *id != NONE) {
            self.damage.push(self.nodes[id as usize].screen);
            self.free_subtree(id);
        }
        for pair in new.windows(2) {
            self.nodes[pair[0] as usize].next = pair[1];
        }
        if let Some(&last) = new.last() {
            self.nodes[last as usize].next = NONE;
        }
        self.nodes[parent as usize].first_child = new.first().copied().unwrap_or(NONE);
        if new != was {
            self.layout_dirty = true;
        }
    }

    fn apply_common<M: Copy>(&mut self, id: NodeId, element: &Element<'_, M>) {
        let node = &mut self.nodes[id as usize];
        if !node.style.layout_eq(&element.style) {
            self.layout_dirty = true;
        }
        if node.style != element.style {
            node.paint_dirty = true;
        }
        node.style = element.style;
        node.key = element.key;
        node.id = element.id;
        node.role = element.role;
        node.tap = element.tap;
    }

    fn update_node<M: Copy>(&mut self, owner: u16, id: NodeId, element: &Element<'_, M>) {
        self.apply_common(id, element);
        self.nodes[id as usize].owner = owner;
        let node = &mut self.nodes[id as usize];
        match (&mut node.content, &element.content) {
            (NodeContent::Text(text), Content::Text { font, text: string, color, options }) => {
                let same_options = text.options.wrap == options.wrap
                    && text.options.max_lines == options.max_lines
                    && text.options.overflow == options.overflow
                    && text.options.align == options.align;
                if text.string != string.as_bytes() || !core::ptr::eq(text.font, *font) || !same_options {
                    text.string.clear();
                    text.string.extend_from_slice(string.as_bytes());
                    text.color = *color;
                    node.paint_dirty = true;
                    let same_font = core::ptr::eq(text.font, *font) && same_options;
                    let size = (text.run.width(), text.run.height());
                    text.font = font;
                    text.options = *options;
                    let width_free = node.style.grow > 0 || matches!(node.style.width, Dimension::Px(_));
                    if let (true, Some(width)) = (same_font, text.laid_out) {
                        let mut options = *options;
                        options.max_width = (width != i16::MAX).then_some(width);
                        text.run.layout(font, string, &options);
                        let same_size = text.run.height() == size.1 && (width_free || text.run.width() == size.0);
                        self.layout_dirty |= !same_size;
                    } else {
                        text.laid_out = None;
                        self.layout_dirty = true;
                    }
                } else if text.color != *color {
                    text.color = *color;
                    node.paint_dirty = true;
                }
            }
            (NodeContent::Image { image, tint }, Content::Image { image: new, tint: new_tint }) => {
                let same = core::ptr::eq(image.pixels(), new.pixels());
                let same_size = image.width() == new.width() && image.height() == new.height();
                if !same || *tint != *new_tint {
                    *image = *new;
                    *tint = *new_tint;
                    node.paint_dirty = true;
                }
                if !same_size {
                    self.layout_dirty = true;
                }
            }
            (NodeContent::List(list), Content::List { id: list_id, count, row_height, revision }) => {
                list.stale |= revision.is_none() || *revision != list.revision;
                list.revision = *revision;
                if list.count != *count || list.row_height != *row_height {
                    list.count = *count;
                    list.row_height = *row_height;
                    node.paint_dirty = true;
                    self.layout_dirty = true;
                }
                list.id = *list_id;
            }
            (NodeContent::Child(slot), Content::Child(spec)) => {
                let slot = *slot;
                let index = usize::from(slot);
                self.slots[index].events = spec.event_map(owner);
                let rebuild = match self.slots[index].host.as_mut() {
                    Some(host) => spec.update(&mut **host),
                    None => false,
                };
                if rebuild {
                    self.build_component(slot);
                }
                return;
            }
            _ => {}
        }
        if !element.children.is_empty() || self.nodes[id as usize].first_child != NONE {
            self.reconcile_children(owner, id, element.children);
        }
    }

    fn create_node<M: Copy>(&mut self, owner: u16, element: &Element<'_, M>) -> NodeId {
        let content = match element.content {
            Content::Empty | Content::Box => NodeContent::Box,
            Content::Text { font, text, color, options } => {
                let run = devkit_gfx::TextRun::new_in(self.caps);
                let mut string = Vec::with_capacity_in(text.len(), self.caps);
                string.extend_from_slice(text.as_bytes());
                NodeContent::Text(TextNode { font, string, color, options, run, laid_out: None })
            }
            Content::Image { image, tint } => NodeContent::Image { image, tint },
            Content::Scroll => NodeContent::Scroll(ScrollNode { offset: 0, content: 0 }),
            Content::List { id, count, row_height, revision } => {
                NodeContent::List(ListNode { id, count, row_height, revision, offset: 0, rows: Vec::new_in(self.caps), stale: false })
            }
            Content::Child(_) => NodeContent::Box,
        };
        let id = self.alloc_node(Node::new(content, owner));
        self.layout_dirty = true;
        self.apply_common(id, element);
        match element.content {
            Content::List { .. } => self.lists.push(id),
            Content::Child(spec) => {
                let host = spec.create();
                let slot = match self.slots.iter().position(|s| s.node == NONE) {
                    Some(free) => free,
                    None => {
                        self.slots.push(Slot { host: None, node: NONE, events: None });
                        self.slots.len() - 1
                    }
                };
                self.slots[slot] = Slot { host: Some(host), node: id, events: spec.event_map(owner) };
                self.nodes[id as usize].content = NodeContent::Child(slot as u16);
                self.build_component(slot as u16);
                return id;
            }
            _ => {}
        }
        if !element.children.is_empty() {
            self.reconcile_children(owner, id, element.children);
        }
        id
    }

    /// Builds the rows a list shows, given its current size and offset.
    fn sync_list(&mut self, id: NodeId) {
        let NodeContent::List(list) = &mut self.nodes[id as usize].content else { return };
        let (row_height, count, list_id) = (i32::from(list.row_height), list.count as i32, list.id);
        let offset = list.offset;
        let stale = core::mem::take(&mut list.stale);
        let viewport = i32::from(self.nodes[id as usize].rect.h);
        let first = (offset / row_height - OVERSCAN_ROWS).max(0);
        let last = ((offset + viewport) / row_height + OVERSCAN_ROWS).min(count - 1);
        let NodeContent::List(list) = &mut self.nodes[id as usize].content else { return };
        let mut rows = core::mem::replace(&mut list.rows, Vec::new_in(self.caps));
        rows.retain(|&(index, row)| {
            let keep = (first..=last).contains(&(index as i32));
            if !keep {
                self.free_subtree(row);
            }
            keep
        });
        let owner = self.nodes[id as usize].owner;
        let width = self.nodes[id as usize].rect.w;
        for index in first..=last {
            let found = rows.binary_search_by_key(&(index as u32), |r| r.0);
            let created = found.is_err();
            let row = match found {
                Ok(at) => rows[at].1,
                Err(at) => {
                    let row = self.alloc_node(Node::new(NodeContent::Box, owner));
                    self.nodes[row as usize].parent = id;
                    rows.insert(at, (index as u32, row));
                    row
                }
            };
            if stale || created {
                let outer_layout = core::mem::take(&mut self.layout_dirty);
                let host = self.slots[usize::from(owner)].host.take();
                if let Some(host) = host {
                    self.depth += 1;
                    host.build_row(self, owner, list_id, index as usize, row);
                    self.depth -= 1;
                    self.slots[usize::from(owner)].host = Some(host);
                }
                if core::mem::replace(&mut self.layout_dirty, outer_layout) {
                    self.clear_measured(row);
                    self.arrange(row, Rect::new(0, 0, width, row_height as i16));
                }
            }
        }
        let NodeContent::List(list) = &mut self.nodes[id as usize].content else { return };
        list.rows = rows;
        if self.depth == 0 {
            self.bump.reset();
        }
    }

    fn clear_measured(&mut self, id: NodeId) {
        self.nodes[id as usize].measured = None;
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            self.clear_measured(child);
            child = self.nodes[child as usize].next;
        }
    }

    fn dispatch(&mut self, slot: u16, msg: Erased) {
        let index = usize::from(slot);
        let Some(mut host) = self.slots[index].host.take() else { return };
        let update = host.dispatch(msg);
        self.slots[index].host = Some(host);
        if update.rebuild && !self.dirty.contains(&slot) {
            self.dirty.push(slot);
        }
        self.queue.extend(update.sends.into_iter().map(|m| (slot, m)));
        if let Some(map) = &self.slots[index].events {
            for event in update.events {
                let parent = unsafe { (map.call)(map.function, event) };
                self.queue.push_back((map.parent, parent));
            }
        }
    }

    pub(crate) fn post(&mut self, slot: u16, msg: Erased) {
        self.queue.push_back((slot, msg));
    }

    /// Applies what happened since the last call, leaving the damage to take.
    pub(crate) fn update(&mut self) {
        while let Some((slot, msg)) = self.queue.pop_front() {
            self.dispatch(slot, msg);
        }
        while let Some(slot) = self.dirty.pop() {
            self.build_component(slot);
        }
        if core::mem::take(&mut self.layout_dirty) {
            self.clear_measured_all();
            let area = self.area;
            self.arrange(self.root, area);
        }
        for i in 0..self.lists.len() {
            self.sync_list(self.lists[i]);
        }
        self.sync_screen(self.root, 0, 0, self.area, false);
    }

    fn clear_measured_all(&mut self) {
        for node in &mut self.nodes {
            node.measured = None;
        }
    }

    fn sync_screen(&mut self, id: NodeId, dx: i32, dy: i32, clip: Rect, quiet: bool) {
        let node = &mut self.nodes[id as usize];
        let (x, y) = (dx + i32::from(node.rect.x), dy + i32::from(node.rect.y));
        let visible = y < i32::from(clip.bottom()) && y + i32::from(node.rect.h) > i32::from(clip.y);
        let screen = if visible { Rect::new(x as i16, y as i16, node.rect.w, node.rect.h) } else { Rect::default() };
        let quiet = quiet || core::mem::take(&mut node.scrolled);
        let moved = node.screen != screen;
        if !quiet && (moved || node.paint_dirty) {
            let extent = node.style.shadow.map_or(0, |s| i16::from(s.blur) + i16::from(s.y.unsigned_abs()));
            let old = node.screen.expand(extent);
            let new = screen.expand(extent);
            self.damage.push(old);
            self.damage.push(new);
        }
        let node = &mut self.nodes[id as usize];
        node.screen = screen;
        node.paint_dirty = false;
        if !visible {
            return;
        }
        let [left, top, _, _] = node.style.padding;
        let (cx, cy) = (x + i32::from(left), y + i32::from(top));
        let viewport = screen.intersect(&clip);
        match &node.content {
            NodeContent::Scroll(scroll) => {
                let offset = scroll.offset;
                self.sync_children(id, cx, cy - offset, viewport, quiet);
            }
            NodeContent::List(list) => {
                let (rows, row_height, offset) = (list.rows.clone(), i32::from(list.row_height), list.offset);
                for (index, row) in rows {
                    self.sync_screen(row, cx, cy + index as i32 * row_height - offset, viewport, quiet);
                }
            }
            _ => self.sync_children(id, x, y, clip, quiet),
        }
    }

    fn sync_children(&mut self, id: NodeId, dx: i32, dy: i32, clip: Rect, quiet: bool) {
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            self.sync_screen(child, dx, dy, clip, quiet);
            child = self.nodes[child as usize].next;
        }
    }

    pub(crate) fn take_damage(&mut self) -> Vec<Rect> {
        core::mem::take(&mut self.damage)
    }

    pub(crate) fn idle(&self) -> bool {
        self.queue.is_empty()
            && self.dirty.is_empty()
            && self.fling.is_none()
            && self.contact.is_none()
            && self.damage.is_empty()
            && !self.layout_dirty
    }

    pub(crate) fn animating(&self) -> bool {
        self.fling.is_some()
    }

    fn scroll_by(&mut self, id: NodeId, delta: i32) -> bool {
        let viewport = i32::from(self.nodes[id as usize].rect.h);
        let (offset, max) = match &mut self.nodes[id as usize].content {
            NodeContent::Scroll(s) => (&mut s.offset, s.content - viewport),
            NodeContent::List(l) => (&mut l.offset, l.count as i32 * i32::from(l.row_height) - viewport),
            _ => return false,
        };
        let next = (*offset + delta).clamp(0, max.max(0));
        let moved = next != *offset;
        *offset = next;
        if moved {
            let node = &mut self.nodes[id as usize];
            node.scrolled = true;
            let screen = node.screen;
            self.damage.push(screen);
        }
        moved
    }

    fn hit(&self, id: NodeId, dx: i32, dy: i32, x: i32, y: i32) -> Option<Hit> {
        let node = &self.nodes[id as usize];
        let (left, top) = (dx + i32::from(node.rect.x), dy + i32::from(node.rect.y));
        let right = left + i32::from(node.rect.w);
        let bottom = top + i32::from(node.rect.h);
        let inside = x >= left && x < right && y >= top && y < bottom;
        let clips = node.style.clip || matches!(node.content, NodeContent::Scroll(_) | NodeContent::List(_));
        if !inside && clips {
            return None;
        }
        let [pad_left, pad_top, _, _] = node.style.padding;
        let mut found = match &node.content {
            NodeContent::List(list) => list.rows.iter().rev().find_map(|&(index, row)| {
                let row_y = top + i32::from(pad_top) + index as i32 * i32::from(list.row_height) - list.offset;
                self.hit(row, left + i32::from(pad_left), row_y, x, y)
            }),
            NodeContent::Scroll(scroll) => {
                self.hit_children(id, left + i32::from(pad_left), top + i32::from(pad_top) - scroll.offset, x, y)
            }
            _ => self.hit_children(id, left, top, x, y),
        };
        if inside {
            let scrollable = matches!(node.content, NodeContent::Scroll(_) | NodeContent::List(_));
            if let Some(hit) = &mut found {
                if hit.scroller == NONE && scrollable {
                    hit.scroller = id;
                }
            } else if node.tap.is_some() {
                found = Some(Hit { tap: id, scroller: if scrollable { id } else { NONE } });
            } else if scrollable {
                found = Some(Hit { tap: NONE, scroller: id });
            }
        }
        found
    }

    fn hit_children(&self, id: NodeId, dx: i32, dy: i32, x: i32, y: i32) -> Option<Hit> {
        let mut children = Vec::new();
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            children.push(child);
            child = self.nodes[child as usize].next;
        }
        children.iter().rev().find_map(|&c| self.hit(c, dx, dy, x, y))
    }

    fn set_pressed(&mut self, id: NodeId, pressed: bool) {
        if id != NONE {
            let node = &mut self.nodes[id as usize];
            node.pressed = pressed;
            if node.style.pressed_background.is_some() {
                node.paint_dirty = true;
            }
        }
    }

    /// The primary contact in logical coordinates, `None` when lifted, at the
    /// time it was sampled.
    pub(crate) fn touch(&mut self, point: Option<(i16, i16)>, now_ms: u32) {
        match (self.contact, point) {
            (None, Some(p)) => {
                self.fling = None;
                let hit = self.hit(self.root, 0, 0, i32::from(p.0), i32::from(p.1));
                let hit = hit.unwrap_or(Hit { tap: NONE, scroller: NONE });
                self.set_pressed(hit.tap, true);
                self.contact = Some(Contact {
                    start: p,
                    last: p,
                    last_ms: now_ms,
                    tap: hit.tap,
                    scroller: hit.scroller,
                    dragging: false,
                    velocity: 0.0,
                });
            }
            (Some(mut contact), Some(p)) => {
                let moved = i32::from(p.1) - i32::from(contact.start.1);
                let starting = !contact.dragging && moved.abs() > TAP_SLOP && contact.scroller != NONE;
                if starting {
                    contact.dragging = true;
                    self.set_pressed(contact.tap, false);
                    contact.tap = NONE;
                }
                let delta = i32::from(contact.last.1) - i32::from(p.1);
                if !contact.dragging {
                    contact.last_ms = now_ms;
                } else if delta != 0 {
                    self.scroll_by(contact.scroller, delta);
                    let dt = now_ms.wrapping_sub(contact.last_ms).max(1) as f32;
                    let speed = delta as f32 / dt;
                    let weight = if starting { 1.0 } else { dt / (dt + VELOCITY_TAU_MS) };
                    contact.velocity += (speed - contact.velocity) * weight;
                    contact.last_ms = now_ms;
                }
                contact.last = p;
                self.contact = Some(contact);
            }
            (Some(contact), None) => {
                self.contact = None;
                self.set_pressed(contact.tap, false);
                if contact.dragging {
                    let rest = now_ms.wrapping_sub(contact.last_ms);
                    if rest < FLING_STALE_MS && contact.velocity.abs() > FLING_MIN {
                        self.fling = Some((contact.scroller, contact.velocity));
                    }
                } else if contact.tap != NONE {
                    let node = &self.nodes[contact.tap as usize];
                    if let Some(msg) = node.tap {
                        self.queue.push_back((node.owner, msg));
                    }
                }
            }
            (None, None) => {}
        }
    }

    /// Advances inertial scrolling.
    pub(crate) fn tick(&mut self, dt_ms: u32) {
        let Some((id, velocity)) = self.fling else { return };
        let dt = (dt_ms as f32).min(MAX_FLING_STEP_MS);
        let step = (velocity * dt_ms as f32) as i32;
        let at_end = !self.scroll_by(id, step) && step != 0;
        let velocity = velocity * (1.0 - FLING_DECAY * dt);
        self.fling = (!at_end && velocity.abs() > FLING_MIN).then_some((id, velocity));
    }

    fn walk(&self, id: NodeId, f: &mut impl FnMut(NodeId, &Node)) {
        let node = &self.nodes[id as usize];
        if node.screen.is_empty() && !matches!(node.content, NodeContent::Child(_)) {
            return;
        }
        f(id, node);
        if let NodeContent::List(list) = &node.content {
            for &(_, row) in &list.rows {
                self.walk(row, f);
            }
        }
        let mut child = node.first_child;
        while child != NONE {
            self.walk(child, f);
            child = self.nodes[child as usize].next;
        }
    }

    pub(crate) fn find(&self, name: &str) -> Option<NodeId> {
        let mut found = None;
        self.walk(self.root, &mut |id, node| {
            if found.is_none() && node.id == Some(name) {
                found = Some(id);
            }
        });
        found
    }

    /// One entry per named or meaningful node: `id role x y w h "text"`.
    pub(crate) fn describe(&self) -> String {
        let mut out = String::new();
        self.walk(self.root, &mut |_, node| {
            if node.id.is_none() && node.role == Role::None {
                return;
            }
            let r = node.screen;
            if !out.is_empty() {
                out.push_str("; ");
            }
            let _ = write!(out, "{} {} {} {} {} {}", node.id.unwrap_or("-"), node.role.name(), r.x, r.y, r.w, r.h);
            if let NodeContent::Text(text) = &node.content {
                let _ = write!(out, " {:?}", text.text());
            }
        });
        out
    }

    /// The point a tap on `name` lands on, if it is the topmost target there.
    pub(crate) fn tap_point(&self, name: &str) -> Result<(i16, i16), String> {
        let id = self.find(name).ok_or_else(|| format!("no node {name}"))?;
        let r = self.nodes[id as usize].screen;
        let (x, y) = (r.x + r.w / 2, r.y + r.h / 2);
        let hit = self.hit(self.root, 0, 0, i32::from(x), i32::from(y));
        match hit {
            Some(hit) if hit.tap == id => Ok((x, y)),
            Some(hit) if hit.tap != NONE => Err(format!("{name} is covered")),
            _ => Err(format!("{name} takes no taps")),
        }
    }

    pub(crate) fn invoke(&mut self, name: &str) -> Result<(), String> {
        let id = self.find(name).ok_or_else(|| format!("no node {name}"))?;
        let node = &self.nodes[id as usize];
        let msg = node.tap.ok_or_else(|| format!("{name} takes no taps"))?;
        self.queue.push_back((node.owner, msg));
        Ok(())
    }

}
