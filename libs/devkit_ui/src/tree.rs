/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::TypeId;
use core::cell::Cell;
use core::fmt::Write;
use core::ptr::NonNull;
use core::slice;

use devkit_gfx::Rect;
use idf::heap::Caps;

use crate::bump::Bump;
use crate::component::{Animation, AnyHost, EventMap, Hook, Host, Property, RenderRequest, Requests, Screen, screen_host};
use crate::element::{BuildContext, Content, Drag, Element};
use crate::keyboard::{InputKind, KeyboardMode, KeyboardPanel};
use crate::erased::Erased;
use crate::nav::{NavigatorNode, Overlay, Root, Saved};
use crate::node::{ListNode, NONE, Node, NodeContent, NodeId, ScrollNode, TextNode};
use crate::presenter::Strips;
use crate::style::{Dimension, Insets, Role, ScreenStyle, State};
use crate::text_buffer::TextInput;
use crate::theme::Theme;

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
const CARET_BLINK_MS: u32 = 500;
const REPEAT_DELAY_MS: u32 = 400;
const REPEAT_EVERY_MS: u32 = 60;

pub(crate) struct Slot {
    pub(crate) host: Option<Box<dyn AnyHost>>,
    pub(crate) node: NodeId,
    pub(crate) events: Option<EventMap>,
    pub(crate) theme: &'static Theme,
}

#[derive(Clone, Copy)]
struct Contact {
    /// The controller's id for this finger.
    track: u8,
    /// Landed on something another finger holds, and acts on nothing.
    passive: bool,
    start: (i16, i16),
    last: (i16, i16),
    /// When the contact last moved; a sample at the same place is not news.
    last_ms: u32,
    tap: NodeId,
    scroller: NodeId,
    drag: NodeId,
    dragging: bool,
    velocity: f32,
    held_ms: u32,
    repeated: bool,
}

#[derive(Clone, Copy)]
struct Hit {
    tap: NodeId,
    scroller: NodeId,
    drag: NodeId,
}

/// Everything below the display: components, nodes, layout and gestures.
pub(crate) struct Tree {
    pub(crate) nodes: Vec<Node, Caps>,
    free: Vec<NodeId>,
    pub(crate) root: NodeId,
    pub(crate) area: Rect,
    pub(crate) slots: Vec<Slot>,
    lists: Vec<NodeId>,
    pub(crate) navigators: Vec<NodeId>,
    pub(crate) root_navigator: NodeId,
    pub(crate) chrome: Vec<u16>,
    pub(crate) restoring: Vec<Saved>,
    pub(crate) theme: &'static Theme,
    pub(crate) screen_style: ScreenStyle,
    insets: Insets,
    pub(crate) render_request: Option<(u16, RenderRequest)>,
    pending_animations: Vec<(&'static str, Animation)>,
    pending_focus: Option<&'static str>,
    focus: NodeId,
    keyboard_changed: bool,
    caret_on: bool,
    caret_ms: u32,
    keyboard_slot: Option<u16>,
    keyboard_height: i16,
    keyboard_mode: KeyboardMode,
    keyboard_connected: bool,
    animations: Vec<(NodeId, Animation, u32)>,
    parked_strips: Option<Strips>,
    bump: Box<Bump>,
    caps: Caps,
    depth: u32,
    queue: VecDeque<(u16, Erased)>,
    dirty: Vec<u16>,
    pub(crate) layout_dirty: bool,
    pub(crate) damage: Vec<Rect>,
    contacts: Vec<Contact>,
    flings: Vec<(NodeId, f32)>,
}

impl Tree {
    pub(crate) fn new<S: Screen>(root: S, area: Rect, caps: Caps, theme: &'static Theme) -> Self {
        let mut tree = Self {
            nodes: Vec::new_in(caps),
            free: Vec::new(),
            root: NONE,
            area,
            slots: Vec::new(),
            lists: Vec::new(),
            navigators: Vec::new(),
            root_navigator: NONE,
            chrome: Vec::new(),
            restoring: Vec::new(),
            theme,
            screen_style: ScreenStyle::default(),
            insets: Insets::default(),
            render_request: None,
            pending_animations: Vec::new(),
            pending_focus: None,
            focus: NONE,
            keyboard_changed: false,
            caret_on: true,
            caret_ms: 0,
            keyboard_slot: None,
            keyboard_height: 0,
            keyboard_mode: KeyboardMode::Auto,
            keyboard_connected: false,
            animations: Vec::new(),
            parked_strips: None,
            bump: Box::new(Bump::new(caps)),
            caps,
            depth: 0,
            queue: VecDeque::new(),
            dirty: Vec::new(),
            layout_dirty: true,
            damage: Vec::new(),
            contacts: Vec::new(),
            flings: Vec::new(),
        };
        tree.root = tree.alloc_node(Node::new(NodeContent::Box, 0));
        let nav = tree.new_navigator(TypeId::of::<Root>(), 0);
        tree.root_navigator = nav;
        tree.add_plane(nav);
        tree.seed_navigator(nav, screen_host(root));
        let overlay = tree.new_navigator(TypeId::of::<Overlay>(), 0);
        tree.add_plane(overlay);
        tree
    }

    /// Stacks a child over the whole area, above the planes so far.
    fn add_plane(&mut self, node: NodeId) {
        self.nodes[node as usize].style.absolute = Some(crate::style::Inset {
            left: Some(0),
            top: Some(0),
            right: Some(0),
            bottom: Some(0),
        });
        self.nodes[node as usize].parent = self.root;
        let mut child = self.nodes[self.root as usize].first_child;
        if child == NONE {
            self.nodes[self.root as usize].first_child = node;
            return;
        }
        while self.nodes[child as usize].next != NONE {
            child = self.nodes[child as usize].next;
        }
        self.nodes[child as usize].next = node;
    }

    /// Adds a component above the screens that stays across navigation, and
    /// the space it covers. Must come before the first update.
    pub(crate) fn add_chrome<C: crate::component::Component>(&mut self, component: C, insets: Insets) -> u16 {
        let slot = self.alloc_slot();
        let node = self.alloc_node(Node::new(NodeContent::Child(slot), slot));
        let index = usize::from(slot);
        self.slots[index].host = Some(Box::new(Host::<C> { component, props: None }));
        self.slots[index].node = node;
        self.slots[index].theme = self.theme;
        self.chrome.push(slot);
        self.insets = insets;
        let overlay = self.navigators[self.navigators.len() - 1];
        self.nodes[node as usize].style.absolute = Some(crate::style::Inset {
            left: Some(0),
            top: Some(0),
            right: Some(0),
            bottom: None,
        });
        self.nodes[node as usize].parent = self.root;
        let mut child = self.nodes[self.root as usize].first_child;
        while self.nodes[child as usize].next != overlay {
            child = self.nodes[child as usize].next;
        }
        self.nodes[child as usize].next = node;
        self.nodes[node as usize].next = overlay;
        self.mark_all_dirty();
        slot
    }

    fn mark_all_dirty(&mut self) {
        for slot in 0..self.slots.len() {
            if self.slots[slot].host.is_some() {
                self.mark_slot_dirty(slot as u16);
            }
        }
    }

    pub(crate) fn mark_slot_dirty(&mut self, slot: u16) {
        if !self.dirty.contains(&slot) {
            self.dirty.push(slot);
        }
    }

    pub(crate) fn context(&self, slot: u16) -> BuildContext {
        BuildContext {
            theme: self.slots[usize::from(slot)].theme,
            insets: Insets { bottom: self.insets.bottom.max(self.keyboard_height), ..self.insets },
            area: self.area,
            screen_style: self.screen_style,
        }
    }

    pub(crate) fn alloc_slot(&mut self) -> u16 {
        let free = self.slots.iter().position(|s| s.node == NONE);
        match free {
            Some(slot) => slot as u16,
            None => {
                self.slots.push(Slot { host: None, node: NONE, events: None, theme: self.theme });
                (self.slots.len() - 1) as u16
            }
        }
    }

    pub(crate) fn run_hook(&mut self, slot: u16, hook: Hook) {
        let index = usize::from(slot);
        let Some(mut host) = self.slots[index].host.take() else { return };
        let requests = host.hook(hook);
        self.slots[index].host = Some(host);
        self.apply_requests(slot, requests);
    }

    pub(crate) fn bump(&self) -> NonNull<Bump> {
        NonNull::from(&*self.bump)
    }

    pub(crate) fn alloc_node(&mut self, node: Node) -> NodeId {
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

    pub(crate) fn free_subtree(&mut self, id: NodeId) {
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
            NodeContent::Navigator(_) => self.navigators.retain(|n| *n != id),
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
        for contact in &mut self.contacts {
            if contact.tap == id {
                contact.tap = NONE;
            }
            if contact.scroller == id {
                contact.scroller = NONE;
            }
            if contact.drag == id {
                contact.drag = NONE;
            }
        }
        self.flings.retain(|f| f.0 != id);
        self.animations.retain(|a| a.0 != id);
        if self.focus == id {
            self.focus = NONE;
            self.keyboard_changed = true;
        }
    }

    pub(crate) fn build_component(&mut self, slot: u16) {
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
        node.drag = element.drag;
        node.input = element.input;
        node.focusable = element.focusable;
        node.keep_focus = element.keep_focus;
        node.keyboard = element.keyboard;
        node.input_kind = element.input_kind;
        node.repeat = element.repeat;
        let kept = node.state & (State::PRESSED | State::HOVERED | State::FOCUSED);
        let state = kept | element.state;
        if state != node.state {
            node.state = state;
            node.paint_dirty = true;
        }
    }

    fn update_node<M: Copy>(&mut self, owner: u16, id: NodeId, element: &Element<'_, M>) {
        self.apply_common(id, element);
        self.nodes[id as usize].owner = owner;
        let node = &mut self.nodes[id as usize];
        match (&mut node.content, &element.content) {
            (NodeContent::Text(text), Content::Text { font, text: string, color, options, edit }) => {
                if text.edit != *edit {
                    text.edit = *edit;
                    node.paint_dirty = true;
                }
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
                    let width_free = node.style.grow > 0 || !matches!(node.style.width, Dimension::Auto);
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
            (NodeContent::Navigator(_), _) => return,
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
            Content::Text { font, text, color, options, edit } => {
                let run = devkit_gfx::TextRun::new_in(self.caps);
                let mut string = Vec::with_capacity_in(text.len(), self.caps);
                string.extend_from_slice(text.as_bytes());
                NodeContent::Text(TextNode { font, string, color, options, run, laid_out: None, edit, scroll: Cell::new(0) })
            }
            Content::Image { image, tint } => NodeContent::Image { image, tint },
            Content::Scroll => NodeContent::Scroll(ScrollNode { offset: 0, content: 0 }),
            Content::List { id, count, row_height, revision } => {
                NodeContent::List(ListNode { id, count, row_height, revision, offset: 0, rows: Vec::new_in(self.caps), stale: false })
            }
            Content::Child(_) => NodeContent::Box,
            Content::Navigator { key, .. } => NodeContent::Navigator(NavigatorNode { key, stack: Vec::new(), slide: None }),
        };
        let id = self.alloc_node(Node::new(content, owner));
        self.layout_dirty = true;
        self.apply_common(id, element);
        match element.content {
            Content::List { id: list_id, .. } => {
                self.lists.push(id);
                let saved = self.restoring.last().and_then(|s| s.lists.iter().find(|l| l.0 == list_id));
                if let (Some(&(_, offset)), NodeContent::List(list)) = (saved, &mut self.nodes[id as usize].content) {
                    list.offset = offset;
                }
            }
            Content::Navigator { key, initial } => {
                self.navigators.push(id);
                if !self.restore_navigator(id, key)
                    && let Some(spec) = initial
                {
                    self.seed_navigator(id, spec.create());
                }
                return id;
            }
            Content::Child(spec) => {
                let host = spec.create();
                let slot = self.alloc_slot();
                let theme = self.slots[usize::from(owner)].theme;
                self.slots[usize::from(slot)] = Slot { host: Some(host), node: id, events: spec.event_map(owner), theme };
                self.nodes[id as usize].content = NodeContent::Child(slot);
                self.build_component(slot);
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

    fn apply_requests(&mut self, slot: u16, requests: Requests) {
        if requests.rebuild {
            self.mark_slot_dirty(slot);
        }
        self.queue.extend(requests.sends.into_iter().map(|m| (slot, m)));
        if let Some(map) = &self.slots[usize::from(slot)].events {
            for event in requests.events {
                let parent = unsafe { (map.call)(map.function, event) };
                self.queue.push_back((map.parent, parent));
            }
        }
        if let Some(theme) = requests.theme {
            self.theme = theme;
            for s in &mut self.slots {
                s.theme = theme;
            }
            self.mark_all_dirty();
        }
        if let Some(render) = requests.render {
            self.render_request = Some((slot, render));
        }
        self.pending_animations.extend(requests.animations);
        if requests.focus.is_some() {
            self.pending_focus = requests.focus;
        }
        for input in requests.input {
            self.input(input);
        }
        for op in requests.nav {
            self.apply_nav(slot, op);
        }
    }

    fn dispatch(&mut self, slot: u16, msg: Erased) {
        let index = usize::from(slot);
        let Some(mut host) = self.slots[index].host.take() else { return };
        let requests = host.dispatch(msg, self.parked_strips.take());
        self.slots[index].host = Some(host);
        self.apply_requests(slot, requests);
    }

    /// A render mode was applied: `done` arrives with the memory `strips`.
    pub(crate) fn render_done(&mut self, slot: u16, done: Option<Erased>, strips: Strips) {
        self.parked_strips = Some(strips);
        if let Some(done) = done {
            self.queue.push_back((slot, done));
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
        if let Some(name) = self.pending_focus.take() {
            match self.find(name) {
                Some(id) => self.set_focus(id),
                None => idf::log::error!(c"ui", "focus: no node {name}"),
            }
        }
        if core::mem::take(&mut self.keyboard_changed) {
            self.sync_keyboard();
            self.update();
            return;
        }
        if !self.pending_animations.is_empty() {
            self.start_animations();
            self.sync_screen(self.root, 0, 0, self.area, false);
        }
    }

    #[cfg(test)]
    pub(crate) fn dispatch_animation(&mut self, name: &'static str, animation: Animation) {
        self.pending_animations.push((name, animation));
    }

    fn start_animations(&mut self) {
        for (name, animation) in core::mem::take(&mut self.pending_animations) {
            match self.find(name) {
                Some(id) => {
                    self.animations.retain(|a| a.0 != id || a.1.property != animation.property);
                    self.animations.push((id, animation, 0));
                    self.apply_animation(id, &animation, 0);
                }
                None => idf::log::error!(c"ui", "animate: no node {name}"),
            }
        }
    }

    fn apply_animation(&mut self, id: NodeId, animation: &Animation, elapsed: u32) {
        let t = elapsed as f32 / f32::from(animation.duration_ms.max(1));
        let eased = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
        let range = f32::from(animation.to) - f32::from(animation.from);
        let value = animation.from + (range * eased) as i16;
        let node = &mut self.nodes[id as usize];
        match animation.property {
            Property::TranslateX => node.translate.0 = value,
            Property::TranslateY => node.translate.1 = value,
            Property::Opacity => node.opacity = Some(value.clamp(0, 255) as u8),
        }
        node.paint_dirty = true;
    }

    fn tick_animations(&mut self, dt_ms: u32) {
        let mut i = 0;
        while let Some(&(id, animation, elapsed)) = self.animations.get(i) {
            let elapsed = (elapsed + dt_ms).min(u32::from(animation.duration_ms));
            self.apply_animation(id, &animation, elapsed);
            if elapsed >= u32::from(animation.duration_ms) {
                if animation.property == Property::Opacity {
                    self.nodes[id as usize].opacity = (animation.to < 255).then_some(animation.to.clamp(0, 255) as u8);
                }
                self.animations.swap_remove(i);
            } else {
                self.animations[i].2 = elapsed;
                i += 1;
            }
        }
    }

    pub(crate) fn caret_visible(&self, id: NodeId) -> bool {
        self.focus == id && self.caret_on
    }

    pub(crate) fn has_caret(&self) -> bool {
        self.focus != NONE
    }

    fn set_focus(&mut self, id: NodeId) {
        if self.focus == id {
            return;
        }
        if self.focus != NONE {
            let node = &mut self.nodes[self.focus as usize];
            node.state.remove(State::FOCUSED);
            node.paint_dirty = true;
        }
        self.focus = id;
        if id != NONE {
            let node = &mut self.nodes[id as usize];
            node.state.insert(State::FOCUSED);
            node.paint_dirty = true;
        }
        self.caret_on = true;
        self.caret_ms = 0;
        self.keyboard_changed = true;
    }

    pub(crate) fn input(&mut self, input: TextInput) {
        if self.focus == NONE {
            return;
        }
        let node = &self.nodes[self.focus as usize];
        let Some(map) = node.input else { return };
        let msg = unsafe { (map.call)(map.function, input) };
        self.queue.push_back((node.owner, msg));
        self.caret_on = true;
        self.caret_ms = 0;
    }

    /// Adds the docked on-screen keyboard above everything.
    pub(crate) fn add_keyboard(&mut self, mode: KeyboardMode) {
        let slot = self.alloc_slot();
        let node = self.alloc_node(Node::new(NodeContent::Child(slot), slot));
        let index = usize::from(slot);
        self.slots[index].host = Some(Box::new(Host::<KeyboardPanel> { component: KeyboardPanel::docked(), props: None }));
        self.slots[index].node = node;
        self.slots[index].theme = self.theme;
        self.keyboard_slot = Some(slot);
        self.keyboard_mode = mode;
        self.nodes[node as usize].style.absolute = Some(crate::style::Inset { left: Some(0), top: Some(0), right: Some(0), bottom: Some(0) });
        self.nodes[node as usize].parent = self.root;
        let mut child = self.nodes[self.root as usize].first_child;
        while self.nodes[child as usize].next != NONE {
            child = self.nodes[child as usize].next;
        }
        self.nodes[child as usize].next = node;
        self.mark_slot_dirty(slot);
        self.keyboard_changed = true;
    }

    pub(crate) fn set_keyboard_connected(&mut self, connected: bool) {
        self.keyboard_connected = connected;
        self.keyboard_changed = true;
    }

    pub(crate) fn set_keyboard_mode(&mut self, mode: KeyboardMode) {
        self.keyboard_mode = mode;
        self.keyboard_changed = true;
    }

    fn sync_keyboard(&mut self) {
        let Some(slot) = self.keyboard_slot else { return };
        let index = usize::from(slot);
        let theme = self.slots[index].theme;
        let focused = (self.focus != NONE).then(|| &self.nodes[self.focus as usize]);
        let wanted = focused.filter(|n| n.input.is_some()).map(|n| {
            n.keyboard.unwrap_or(match n.input_kind {
                InputKind::Text => theme.keyboards.text,
                InputKind::Number => theme.keyboards.number,
            })
        });
        let layout = match self.keyboard_mode {
            KeyboardMode::Never => None,
            KeyboardMode::Always => Some(wanted.unwrap_or(theme.keyboards.text)),
            KeyboardMode::Auto => wanted.filter(|_| !self.keyboard_connected),
        };
        let Some(host) = self.slots[index].host.as_mut() else { return };
        let Some(panel) = host.as_any().downcast_mut::<Host<KeyboardPanel>>() else { return };
        panel.component.show(layout);
        self.mark_slot_dirty(slot);
        let height = layout.map_or(0, |l| KeyboardPanel::height(l, theme));
        if height != self.keyboard_height {
            self.keyboard_height = height;
            self.mark_all_dirty();
        }
    }

    fn clear_measured_all(&mut self) {
        for node in &mut self.nodes {
            node.measured = None;
        }
    }

    fn sync_screen(&mut self, id: NodeId, dx: i32, dy: i32, clip: Rect, quiet: bool) {
        let node = &mut self.nodes[id as usize];
        let x = dx + i32::from(node.rect.x) + i32::from(node.translate.0);
        let y = dy + i32::from(node.rect.y) + i32::from(node.translate.1);
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
            && self.flings.is_empty()
            && !self.sliding()
            && self.animations.is_empty()
            && !self.keyboard_changed
            && self.contacts.is_empty()
            && self.damage.is_empty()
            && !self.layout_dirty
    }

    pub(crate) fn animating(&self) -> bool {
        !self.flings.is_empty() || self.sliding() || !self.animations.is_empty()
    }

    pub(crate) fn repeating(&self) -> bool {
        self.contacts.iter().any(|c| self.nodes.get(c.tap as usize).is_some_and(|n| n.repeat))
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
        let left = dx + i32::from(node.rect.x) + i32::from(node.translate.0);
        let top = dy + i32::from(node.rect.y) + i32::from(node.translate.1);
        let right = left + i32::from(node.rect.w);
        let bottom = top + i32::from(node.rect.h);
        let inside = x >= left && x < right && y >= top && y < bottom;
        let clips = node.style.clip || matches!(node.content, NodeContent::Scroll(_) | NodeContent::List(_));
        if !inside && clips {
            return None;
        }
        if inside && node.state.contains(State::DISABLED) {
            return Some(Hit { tap: NONE, scroller: NONE, drag: NONE });
        }
        if matches!(&node.content, NodeContent::Navigator(nav) if nav.slide.is_some()) {
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
            } else if node.drag.is_some() {
                found = Some(Hit { tap: NONE, scroller: NONE, drag: id });
            } else if node.tap.is_some() || node.input.is_some() || node.focusable {
                found = Some(Hit { tap: id, scroller: if scrollable { id } else { NONE }, drag: NONE });
            } else if scrollable {
                found = Some(Hit { tap: NONE, scroller: id, drag: NONE });
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
            node.state.set(State::PRESSED, pressed);
            node.paint_dirty |= node.style.look.is_some();
        }
    }

    /// Every finger on the panel in logical coordinates, with the controller's
    /// track id, at the time they were sampled.
    pub(crate) fn touch(&mut self, points: &[(u8, (i16, i16))], now_ms: u32) {
        let mut i = 0;
        while i < self.contacts.len() {
            if points.iter().any(|p| p.0 == self.contacts[i].track) {
                i += 1;
            } else {
                let contact = self.contacts.swap_remove(i);
                self.release(contact, now_ms);
            }
        }
        for &(track, p) in points {
            match self.contacts.iter().position(|c| c.track == track) {
                Some(index) => self.contact_moved(index, p, now_ms),
                None => self.press(track, p, now_ms),
            }
        }
    }

    fn press(&mut self, track: u8, p: (i16, i16), now_ms: u32) {
        let hit = self.hit(self.root, 0, 0, i32::from(p.0), i32::from(p.1));
        let hit = hit.unwrap_or(Hit { tap: NONE, scroller: NONE, drag: NONE });
        let held = |id: NodeId, of: fn(&Contact) -> NodeId| id != NONE && self.contacts.iter().any(|c| of(c) == id);
        let passive = held(hit.scroller, |c| c.scroller) || held(hit.tap, |c| c.tap) || held(hit.drag, |c| c.drag);
        let hit = if passive { Hit { tap: NONE, scroller: NONE, drag: NONE } } else { hit };
        self.flings.retain(|f| f.0 != hit.scroller);
        self.set_pressed(hit.tap, true);
        self.send_drag(hit.drag, p, false);
        self.contacts.push(Contact {
            track,
            passive,
            start: p,
            last: p,
            last_ms: now_ms,
            tap: hit.tap,
            scroller: hit.scroller,
            drag: hit.drag,
            dragging: false,
            velocity: 0.0,
            held_ms: 0,
            repeated: false,
        });
    }

    fn contact_moved(&mut self, index: usize, p: (i16, i16), now_ms: u32) {
        let mut contact = self.contacts[index];
        self.send_drag(contact.drag, p, false);
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
        } else if delta != 0 && contact.scroller != NONE {
            self.scroll_by(contact.scroller, delta);
            let dt = now_ms.wrapping_sub(contact.last_ms).max(1) as f32;
            let speed = delta as f32 / dt;
            let weight = if starting { 1.0 } else { dt / (dt + VELOCITY_TAU_MS) };
            contact.velocity += (speed - contact.velocity) * weight;
            contact.last_ms = now_ms;
        }
        contact.last = p;
        self.contacts[index] = contact;
    }

    fn release(&mut self, contact: Contact, now_ms: u32) {
        self.set_pressed(contact.tap, false);
        self.send_drag(contact.drag, contact.last, true);
        if contact.dragging {
            let rest = now_ms.wrapping_sub(contact.last_ms);
            if rest < FLING_STALE_MS && contact.velocity.abs() > FLING_MIN && contact.scroller != NONE {
                self.flings.push((contact.scroller, contact.velocity));
            }
        } else if contact.tap != NONE {
            self.finish_tap(contact.tap, contact.last, contact.repeated);
        } else if contact.drag == NONE && !contact.passive {
            self.set_focus(NONE);
        }
    }

    fn finish_tap(&mut self, id: NodeId, point: (i16, i16), repeated: bool) {
        let node = &self.nodes[id as usize];
        let owner = node.owner;
        let tap = node.tap;
        if node.input.is_some() || node.focusable {
            let place = match &node.content {
                NodeContent::Text(text) if node.input.is_some() => {
                    let [left, top, right, _] = node.style.padding;
                    let scroll = text.scroll(node.screen.w - left - right);
                    Some(text.run.byte_at(point.0 - node.screen.x - left + scroll, point.1 - node.screen.y - top))
                }
                _ => None,
            };
            self.set_focus(id);
            if let Some(byte) = place {
                self.input(TextInput::Place(byte as u32));
            }
        } else if !node.keep_focus {
            self.set_focus(NONE);
        }
        if !repeated
            && let Some(msg) = tap
        {
            self.queue.push_back((owner, msg));
        }
    }

    fn tick_repeat(&mut self, dt_ms: u32) {
        let steps = |ms: u32| if ms < REPEAT_DELAY_MS { 0 } else { (ms - REPEAT_DELAY_MS) / REPEAT_EVERY_MS + 1 };
        for contact in &mut self.contacts {
            if contact.tap == NONE || !self.nodes[contact.tap as usize].repeat {
                continue;
            }
            let before = contact.held_ms;
            contact.held_ms += dt_ms;
            let node = &self.nodes[contact.tap as usize];
            if let Some(msg) = node.tap {
                for _ in steps(before)..steps(contact.held_ms) {
                    self.queue.push_back((node.owner, msg));
                    contact.repeated = true;
                }
            }
        }
    }

    fn tick_caret(&mut self, dt_ms: u32) {
        if self.focus == NONE {
            return;
        }
        self.caret_ms += dt_ms;
        if self.caret_ms >= CARET_BLINK_MS {
            self.caret_ms = 0;
            self.caret_on = !self.caret_on;
            self.nodes[self.focus as usize].paint_dirty = true;
        }
    }

    fn send_drag(&mut self, id: NodeId, point: (i16, i16), ended: bool) {
        if id == NONE {
            return;
        }
        let node = &self.nodes[id as usize];
        let Some(map) = node.drag else { return };
        let screen = node.screen;
        let drag = Drag { x: point.0 - screen.x, y: point.1 - screen.y, width: screen.w, ended };
        let msg = unsafe { (map.call)(map.function, drag) };
        self.queue.push_back((node.owner, msg));
    }

    /// Advances what moves by itself.
    pub(crate) fn tick(&mut self, dt_ms: u32) {
        self.tick_slides(dt_ms);
        self.tick_animations(dt_ms);
        self.tick_repeat(dt_ms);
        self.tick_caret(dt_ms);
        let dt = (dt_ms as f32).min(MAX_FLING_STEP_MS);
        let mut i = 0;
        while let Some(&(id, velocity)) = self.flings.get(i) {
            let step = (velocity * dt_ms as f32) as i32;
            let at_end = !self.scroll_by(id, step) && step != 0;
            let velocity = velocity * (1.0 - FLING_DECAY * dt);
            if at_end || velocity.abs() <= FLING_MIN {
                self.flings.swap_remove(i);
            } else {
                self.flings[i].1 = velocity;
                i += 1;
            }
        }
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
            if node.state.contains(State::DISABLED) {
                out.push_str(" disabled");
            }
            if let NodeContent::Text(text) = &node.content {
                let _ = write!(out, " {:?}", text.text());
            }
        });
        out
    }

    /// The point a tap on `name` lands on, if it is the topmost target there.
    pub(crate) fn tap_point(&self, name: &str) -> Result<(i16, i16), String> {
        let id = self.find(name).ok_or_else(|| format!("no node {name}"))?;
        if self.nodes[id as usize].state.contains(State::DISABLED) {
            return Err(format!("{name} is disabled"));
        }
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
        if node.state.contains(State::DISABLED) {
            return Err(format!("{name} is disabled"));
        }
        let msg = node.tap.ok_or_else(|| format!("{name} takes no taps"))?;
        self.queue.push_back((node.owner, msg));
        Ok(())
    }

}
