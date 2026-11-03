/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::TypeId;
use core::cell::Cell;
use core::fmt::Write;
use core::ptr::NonNull;
use core::slice;

use devkit_gfx::{Color, ImageFormat, Rect};
use idf::heap::Caps;
use idf::sync::OnceLock;

use crate::app::Link;
use crate::bump::Bump;
use crate::component::{Animation, AnyHost, EventMap, Hook, Host, Property, RenderRequest, Requests, Screen, screen_host};
use crate::element::{BuildContext, Content, Drag, Element};
use crate::gesture::{Gestures, Tracker};
use crate::external::ExternalSurface;
use crate::keyboard::{InputKind, KeyboardMode, KeyboardPanel};
use crate::erased::Erased;
use crate::motion::{Clock, Motion, MotionKind};
use crate::nav::{NavigatorNode, Overlay, Root, Saved};
use crate::node::{CanvasNode, ListNode, NONE, Node, NodeContent, NodeId, Pull, ScrollNode, TextNode};
use crate::presenter::{ROW_GRID, Strips};
use crate::progress::ProgressNode;
use crate::style::{Dimension, Insets, Role, ScreenStyle, State, Style};
use crate::text_buffer::TextInput;
use crate::theme::{ColorMode, Theme};

const OVERSCAN_ROWS: i32 = 1;
pub(crate) const TAP_SLOP: i32 = 12;
const FLING_MIN: f32 = 0.05;
/// Time constant of the speed average: older movement fades with this.
const VELOCITY_TAU_MS: f32 = 40.0;
/// A finger still for this long before lifting throws nothing.
const FLING_STALE_MS: u32 = 80;
/// Share of the speed lost per millisecond.
const FLING_DECAY: f32 = 0.0025;
const MAX_FLING_STEP_MS: f32 = 32.0;
/// How hard a drag past an end pulls, at the end itself.
const STRETCH: f32 = 0.3;
/// What a drag past an end approaches and never reaches, by the viewport.
const STRETCH_LIMIT: f32 = 1.0 / 3.0;
/// Of the resistance up to a pull's height; 1 is the same as without a pull.
const PULL_RESISTANCE: f32 = 0.5;
/// Per millisecond, of the critically damped spring back to an end.
const SPRING_RATE: f32 = 0.015;
const SETTLE_PX: f32 = 0.5;
const SETTLE_SPEED: f32 = 0.01;
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
pub(crate) struct Contact {
    /// The controller's id for this finger.
    pub(crate) track: u8,
    /// Landed on something another finger holds or was taken by a gesture, and
    /// acts on nothing.
    pub(crate) passive: bool,
    start: (i16, i16),
    pub(crate) last: (i16, i16),
    /// When the contact last moved; a sample at the same place is not news.
    last_ms: u32,
    pub(crate) tap: NodeId,
    pub(crate) scroller: NodeId,
    drag: NodeId,
    pub(crate) gesture: NodeId,
    /// Nothing nearer to the finger takes a pan.
    pub(crate) pan: bool,
    pub(crate) dragging: bool,
    /// Of `scroller`, before rounding.
    position: f32,
    velocity: f32,
    held_ms: u32,
    repeated: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct Hit {
    pub(crate) tap: NodeId,
    scroller: NodeId,
    drag: NodeId,
    gesture: NodeId,
}

const NO_HIT: Hit = Hit { tap: NONE, scroller: NONE, drag: NONE, gesture: NONE };

/// A scroller moving by itself: thrown, or springing back to `anchor` or to
/// the end it is past.
#[derive(Clone, Copy)]
pub(crate) struct Fling {
    position: f32,
    velocity: f32,
    anchor: Option<f32>,
}

#[derive(Clone, Copy)]
struct Bounds {
    start: f32,
    end: f32,
    limit: f32,
    /// Past the start, how far the resistance is `PULL_RESISTANCE`.
    pull: f32,
    past_start: bool,
    past_end: bool,
}

impl Bounds {
    /// The end `position` is past, and whether it may be.
    fn past(&self, position: f32) -> Option<(f32, bool)> {
        if position < self.start {
            Some((self.start, self.past_start))
        } else if position > self.end {
            Some((self.end, self.past_end))
        } else {
            None
        }
    }

    fn drag(&self, position: f32, delta: f32) -> f32 {
        let raw = if position < self.start {
            self.start - self.unstretch(self.start - position, self.pull)
        } else if position > self.end {
            self.end + self.unstretch(position - self.end, 0.0)
        } else {
            position
        } + delta;
        if raw < self.start {
            if self.past_start { self.start - self.stretch(self.start - raw, self.pull) } else { self.start }
        } else if raw > self.end {
            if self.past_end { self.end + self.stretch(raw - self.end, 0.0) } else { self.end }
        } else {
            raw
        }
    }

    fn stretch(&self, raw: f32, pull: f32) -> f32 {
        let knee = PULL_RESISTANCE * unstretch(pull, self.limit);
        if raw < knee {
            stretch(raw / PULL_RESISTANCE, self.limit)
        } else {
            stretch(raw - knee + unstretch(pull, self.limit), self.limit)
        }
    }

    fn unstretch(&self, past: f32, pull: f32) -> f32 {
        if past < pull {
            PULL_RESISTANCE * unstretch(past, self.limit)
        } else {
            (PULL_RESISTANCE - 1.0) * unstretch(pull, self.limit) + unstretch(past, self.limit)
        }
    }
}

fn stretch(raw: f32, limit: f32) -> f32 {
    limit * (1.0 - 1.0 / (raw * STRETCH / limit + 1.0))
}

fn unstretch(past: f32, limit: f32) -> f32 {
    let past = past.min(limit * 0.99);
    limit / STRETCH * past / (limit - past)
}

fn nearest(x: f32) -> i32 {
    (x + if x < 0.0 { -0.5 } else { 0.5 }) as i32
}

// `core` has no `exp`, and a call into libm would bring in `double`
// arithmetic. Close enough for x up to the 0.5 a capped step gives.
fn exp_neg(x: f32) -> f32 {
    1.0 / (1.0 + x * (1.0 + x * (0.5 + x * (1.0 / 6.0 + x / 24.0))))
}

/// Everything below the display: components, nodes, layout and gestures.
pub(crate) struct Tree {
    pub(crate) nodes: Vec<Node, Caps>,
    free: Vec<NodeId>,
    pub(crate) root: NodeId,
    pub(crate) area: Rect,
    pub(crate) slots: Vec<Slot>,
    lists: Vec<NodeId>,
    canvases: Vec<NodeId>,
    external: NodeId,
    pub(crate) external_split: Cell<usize>,
    /// A navigator, and where the display list is once it is painted.
    pub(crate) zoom_split: Cell<(NodeId, usize)>,
    pending_redraw: Vec<(&'static str, Option<Rect>)>,
    format: ImageFormat,
    pub(crate) navigators: Vec<NodeId>,
    pub(crate) root_navigator: NodeId,
    pub(crate) chrome: Vec<u16>,
    pub(crate) popups: NodeId,
    pub(crate) overlay: NodeId,
    /// The scrim's alpha as last painted.
    pub(crate) scrim: u8,
    pub(crate) motions: Vec<Motion>,
    pub(crate) ended_motions: Vec<Motion>,
    pub(crate) ticking: bool,
    /// Created with `.appear`, waiting to be placed.
    pub(crate) presence_pending: Vec<NodeId>,
    /// Painted for a picture, as if shown.
    pub(crate) capturing: Cell<NodeId>,

    pub(crate) restoring: Vec<Saved>,
    pub(crate) theme: &'static Theme,
    pub(crate) screen_style: ScreenStyle,
    pub(crate) color_mode: ColorMode,
    pub(crate) link: Link,
    pub(crate) root_id: u32,
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
    /// The next layout is a new screen size, and nothing eases into it.
    resized: bool,
    pub(crate) parked_strips: Option<Strips>,
    bump: Box<Bump>,
    pub(crate) caps: Caps,
    depth: u32,
    pub(crate) queue: VecDeque<(u16, Erased, Option<Rect>)>,
    dirty: Vec<u16>,
    pub(crate) layout_dirty: bool,
    /// The state of the nearest `state_source` around what is being reconciled.
    source: State,
    pub(crate) damage: Vec<Rect>,
    pub(crate) contacts: Vec<Contact>,
    pub(crate) trackers: Vec<Tracker>,
}

pub(crate) fn shadow_extent(style: &Style) -> i16 {
    style.shadow.map_or(0, |s| i16::from(s.blur) + i16::from(s.y.unsigned_abs()))
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
            popups: NONE,
            overlay: NONE,
            scrim: 0,
            motions: Vec::new(),
            ended_motions: Vec::new(),
            ticking: false,
            presence_pending: Vec::new(),
            capturing: Cell::new(NONE),

            restoring: Vec::new(),
            theme,
            screen_style: ScreenStyle::default(),
            color_mode: ColorMode::Light,
            link: Arc::new(OnceLock::new()),
            root_id: 0,
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
            resized: false,
            parked_strips: None,
            bump: Box::new(Bump::new(caps)),
            caps,
            depth: 0,
            queue: VecDeque::new(),
            dirty: Vec::new(),
            layout_dirty: true,
            source: State::empty(),
            damage: Vec::new(),
            canvases: Vec::new(),
            external: NONE,
            external_split: Cell::new(0),
            zoom_split: Cell::new((NONE, 0)),
            pending_redraw: Vec::new(),
            format: ImageFormat::Rgb565,
            contacts: Vec::new(),
            trackers: Vec::new(),
        };
        tree.root = tree.alloc_node(Node::new(NodeContent::Box, 0, tree.caps));
        let nav = tree.new_navigator(TypeId::of::<Root>(), 0);
        tree.root_navigator = nav;
        tree.add_plane(nav);
        let root = screen_host(root);
        tree.root_id = root.id();
        tree.seed_navigator(nav, root);
        let overlay = tree.new_navigator(TypeId::of::<Overlay>(), 0);
        tree.overlay = overlay;
        tree.add_plane(overlay);
        tree.popups = tree.alloc_node(Node::new(NodeContent::Box, 0, tree.caps));
        tree.add_plane(tree.popups);
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
        let node = self.alloc_node(Node::new(NodeContent::Child(slot), slot, self.caps));
        let index = usize::from(slot);
        self.slots[index].host = Some(Box::new(Host::<C>::new(component, None)));
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

    /// What the planes above the one holding `slot` cover.
    fn insets_above(&self, slot: u16) -> Insets {
        let mut plane = self.slots[usize::from(slot)].node;
        while plane != NONE && self.nodes[plane as usize].parent != self.root {
            plane = self.nodes[plane as usize].parent;
        }
        let chrome = |node: NodeId| self.chrome.iter().any(|&c| self.slots[usize::from(c)].node == node);
        let keyboard = self.keyboard_slot.map_or(NONE, |k| self.slots[usize::from(k)].node);
        // Not placed yet: under everything, as a screen is.
        let mut above = if plane == NONE { self.nodes[self.root as usize].first_child } else { self.nodes[plane as usize].next };
        let mut insets = Insets::default();
        while above != NONE {
            if chrome(above) {
                insets = self.insets;
            }
            if above == keyboard {
                insets.bottom = insets.bottom.max(self.keyboard_height);
            }
            above = self.nodes[above as usize].next;
        }
        insets
    }

    pub(crate) fn context(&self, slot: u16) -> BuildContext {
        let (split_role, can_pop) = self.nav_state(slot);
        let theme = self.slots[usize::from(slot)].theme;
        BuildContext {
            split_role,
            can_pop,
            colors: *theme.palette(self.color_mode),
            color_mode: self.color_mode,
            theme,
            insets: self.insets_above(slot),
            area: self.area,
            screen_style: self.screen_style,
            format: self.format,
        }
    }

    /// Lays everything out again for a turned screen. Fingers on it no longer
    /// mean what they did, so they act on nothing until they lift.
    pub(crate) fn set_area(&mut self, area: Rect) {
        for index in 0..self.contacts.len() {
            let contact = self.contacts[index];
            self.set_pressed(contact.tap, false);
            self.set_pressed(contact.drag, false);
            self.send_drag(contact.drag, contact.last, true);
            let contact = &mut self.contacts[index];
            (contact.tap, contact.scroller, contact.drag, contact.gesture) = (NONE, NONE, NONE, NONE);
            (contact.passive, contact.dragging) = (true, false);
        }
        self.trackers.clear();
        self.motions.retain(|m| !matches!(m.kind, MotionKind::Fling(_)));
        self.close_popups();
        self.area = area;
        self.layout_dirty = true;
        self.resized = true;
        self.keyboard_changed = true;
        self.mark_all_dirty();
    }

    pub(crate) fn set_pixel_format(&mut self, format: ImageFormat) {
        self.format = format;
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
        let requests = host.hook(hook, &self.link);
        self.slots[index].host = Some(host);
        self.apply_requests(slot, requests);
    }

    pub(crate) fn host_id(&self, slot: u16) -> u32 {
        self.slots[usize::from(slot)].host.as_ref().map_or(0, |host| host.id())
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
            NodeContent::Canvas(_) => self.canvases.retain(|c| *c != id),
            NodeContent::External(_) if self.external == id => self.external = NONE,
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
        self.forget_gestures(id);
        self.forget_motions(id);
        self.presence_pending.retain(|p| *p != id);
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
        let source = core::mem::take(&mut self.source);
        host.build(self, slot, node);
        self.source = source;
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
        if node.key != element.key || node.leaving {
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
        let (mut old, mut left) = (Vec::new(), Vec::new());
        let mut child = self.nodes[parent as usize].first_child;
        while child != NONE {
            if self.nodes[child as usize].leaving { left.push(child) } else { old.push(child) }
            child = self.nodes[child as usize].next;
        }
        let mut was = old.clone();
        was.extend_from_slice(&left);
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
            if self.leave(id) {
                left.push(id);
            } else {
                self.damage_painted(id);
                self.free_subtree(id);
            }
        }
        new.extend_from_slice(&left);
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

    // Out of line: reconciling recurses, and two `Style`s would stay on the stack
    // at every level of a tree.
    /// Makes `node.style` what `base` comes to in the node's state.
    #[inline(never)]
    fn restyle(&mut self, id: NodeId, base: Style) {
        let node = &mut self.nodes[id as usize];
        let mut style = base;
        let selected = style.resolve(node.state, &node.when);
        if !node.style.layout_eq(&style) {
            self.layout_dirty = true;
        }
        let changed = node.style != style;
        if changed {
            node.paint_dirty = true;
            if node.style.shadow != style.shadow {
                self.damage.push(node.screen.expand(shadow_extent(&node.style)));
            }
        }
        let old = core::mem::replace(&mut node.style, style);
        match &mut node.base {
            Some(kept) if selected => **kept = base,
            _ => node.base = selected.then(|| Box::new_in(base, self.caps)),
        }
        if changed {
            self.ease_style(id, &old);
        }
    }

    /// Brings a node's state and style up to date with `own_state` and
    /// `source_state`, and passes the state on.
    fn update_state(&mut self, id: NodeId) {
        let node = &mut self.nodes[id as usize];
        let state = node.own_state | node.source_state;
        if state == node.state {
            return;
        }
        node.state = state;
        let base = node.base.as_deref().copied().unwrap_or(node.style);
        let source = node.state_source;
        self.restyle(id, base);
        if source {
            self.pass_state(id, state);
        }
    }

    fn pass_state(&mut self, id: NodeId, state: State) {
        let owner = self.nodes[id as usize].owner;
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            let node = &mut self.nodes[child as usize];
            let (next, source) = (node.next, node.state_source);
            if node.owner == owner {
                node.source_state = state & node.inherit;
                self.update_state(child);
                if !source {
                    self.pass_state(child, state);
                }
            }
            child = next;
        }
    }

    fn set_state(&mut self, id: NodeId, state: State, on: bool) {
        self.nodes[id as usize].own_state.set(state, on);
        self.update_state(id);
    }

    fn reconcile_inside<M: Copy>(&mut self, owner: u16, id: NodeId, element: &Element<'_, M>) {
        let saved = self.source;
        if element.state_source {
            self.source = self.nodes[id as usize].state;
        }
        self.reconcile_children(owner, id, element.children());
        self.source = saved;
    }

    fn apply_common<M: Copy>(&mut self, id: NodeId, element: &Element<'_, M>) {
        let source = self.source;
        let node = &mut self.nodes[id as usize];
        node.key = element.key;
        node.id = element.id;
        node.role = element.role;
        node.tap = element.tap;
        node.drag = element.drag;
        node.on_scroll = element.on_scroll;
        node.bounce = element.bounce;
        node.gestures = element.gestures;
        node.size = element.size;
        node.input = element.input;
        node.focusable = element.focusable;
        node.keep_focus = element.keep_focus;
        node.keyboard = element.keyboard;
        node.input_kind = element.input_kind;
        node.repeat = element.repeat;
        node.appear = element.appear;
        node.disappear = element.disappear;
        node.own_state = (node.own_state & (State::PRESSED | State::HOVERED | State::FOCUSED)) | element.state;
        node.source_state = source & element.inherit;
        node.state = node.own_state | node.source_state;
        node.inherit = element.inherit;
        node.state_source = element.state_source;
        if node.when.as_slice() != element.when {
            node.when.clear();
            node.when.extend_from_slice(element.when);
        }
        self.restyle(id, element.style);
        self.sync_pull(id, element.pull);
    }

    fn update_node<M: Copy>(&mut self, owner: u16, id: NodeId, element: &Element<'_, M>) {
        self.apply_common(id, element);
        self.nodes[id as usize].owner = owner;
        let node = &mut self.nodes[id as usize];
        match (&mut node.content, &element.content) {
            (NodeContent::Text(text), Content::Text { font, text: string, options, edit }) => {
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
            (NodeContent::External(surface), Content::External(new)) => {
                if !surface.same(new) {
                    *surface = (*new).clone();
                }
            }
            (NodeContent::Canvas(canvas), Content::Canvas(canvas_id)) => {
                node.paint_dirty |= canvas.id != *canvas_id;
                canvas.id = *canvas_id;
            }
            (NodeContent::Progress(progress), Content::Progress { gauge, value, period_ms }) => {
                let changed = progress.gauge != *gauge || progress.value != *value || progress.period_ms != *period_ms;
                if changed {
                    (progress.gauge, progress.value, progress.period_ms) = (*gauge, *value, *period_ms);
                    node.paint_dirty = true;
                    self.sync_progress(id);
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
            (NodeContent::Navigator(nav), Content::Navigator { split, transition, .. }) => {
                nav.wanted = *split;
                nav.transition = *transition;
                return;
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
        if !element.children().is_empty() || self.nodes[id as usize].first_child != NONE {
            self.reconcile_inside(owner, id, element);
        }
    }

    // Apart from create_node, which recurses: a `Node` is large, and would stay on
    // the stack at every level of a tree.
    #[inline(never)]
    fn new_node<M: Copy>(&mut self, owner: u16, element: &Element<'_, M>) -> NodeId {
        let content = match element.content {
            Content::Empty | Content::Box => NodeContent::Box,
            Content::Text { font, text, options, edit } => {
                let run = devkit_gfx::TextRun::new_in(self.caps);
                let mut string = Vec::with_capacity_in(text.len(), self.caps);
                string.extend_from_slice(text.as_bytes());
                NodeContent::Text(TextNode { font, string, options, run, laid_out: None, edit, scroll: Cell::new(0) })
            }
            Content::Image { image, tint } => NodeContent::Image { image, tint },
            Content::Scroll => NodeContent::Scroll(ScrollNode { offset: 0, content: 0 }),
            Content::List { id, count, row_height, revision } => {
                NodeContent::List(ListNode { id, count, row_height, revision, offset: 0, rows: Vec::new_in(self.caps), stale: false })
            }
            Content::Canvas(id) => NodeContent::Canvas(CanvasNode { id, reported: (0, 0) }),
            Content::External(surface) => NodeContent::External(surface.clone()),
            Content::Progress { gauge, value, period_ms } => NodeContent::Progress(ProgressNode::new(gauge, value, period_ms)),
            Content::Child(_) => NodeContent::Box,
            Content::Navigator { key, split, transition, .. } => {
                let mut navigator = NavigatorNode::new(key);
                navigator.wanted = split;
                navigator.transition = transition;
                NodeContent::Navigator(navigator)
            }
        };
        let id = self.alloc_node(Node::new(content, owner, self.caps));
        self.layout_dirty = true;
        self.apply_common(id, element);
        id
    }

    fn create_node<M: Copy>(&mut self, owner: u16, element: &Element<'_, M>) -> NodeId {
        let id = self.new_node(owner, element);
        if element.appear.is_some_and(|p| p.moves()) {
            self.presence_pending.push(id);
        }
        match element.content {
            Content::Canvas(_) => self.canvases.push(id),
            Content::Progress { .. } => self.sync_progress(id),
            Content::External(_) => {
                if self.external != NONE {
                    idf::log::error!(c"ui", "only one external surface can show at a time");
                }
                self.external = id;
            }
            Content::List { id: list_id, .. } => {
                self.lists.push(id);
                let saved = self.restoring.last().and_then(|s| s.lists.iter().find(|l| l.0 == list_id));
                if let (Some(&(_, offset)), NodeContent::List(list)) = (saved, &mut self.nodes[id as usize].content) {
                    list.offset = offset;
                }
            }
            Content::Navigator { key, initial, .. } => {
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
        if !element.children().is_empty() {
            self.reconcile_inside(owner, id, element);
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
                    let row = self.alloc_node(Node::new(NodeContent::Box, owner, self.caps));
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
        let tap_rect = requests.tap_rect;
        self.queue.extend(requests.sends.into_iter().map(|m| (slot, m, tap_rect)));
        for (id, msg) in requests.posts {
            self.post(id, msg);
        }
        if let Some(map) = &self.slots[usize::from(slot)].events {
            for event in requests.events {
                let parent = unsafe { (map.call)(map.function, event) };
                self.queue.push_back((map.parent, parent, tap_rect));
            }
        }
        if let Some(theme) = requests.theme {
            self.set_theme(theme);
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
        self.pending_redraw.extend(requests.redraw);
        for op in requests.nav {
            self.apply_nav(slot, op);
        }
        if requests.close_popup {
            self.close_popup(slot);
        }
        for popup in requests.popups {
            self.open_popup(slot, popup);
        }
    }

    pub(crate) fn set_color_mode(&mut self, mode: ColorMode) {
        self.color_mode = mode;
        self.mark_all_dirty();
    }

    pub(crate) fn set_theme(&mut self, theme: &'static Theme) {
        self.theme = theme;
        for s in &mut self.slots {
            s.theme = theme;
        }
        self.mark_all_dirty();
    }

    pub(crate) fn dispatch(&mut self, slot: u16, msg: Erased, tap_rect: Option<Rect>) {
        let index = usize::from(slot);
        let Some(mut host) = self.slots[index].host.take() else { return };
        let requests = host.dispatch(msg, self.parked_strips.take(), tap_rect, &self.link);
        self.slots[index].host = Some(host);
        self.apply_requests(slot, requests);
    }

    /// A render mode was applied: `done` arrives with the memory `strips`.
    pub(crate) fn render_done(&mut self, slot: u16, done: Option<Erased>, strips: Strips) {
        self.parked_strips = Some(strips);
        if let Some(done) = done {
            self.queue.push_back((slot, done, None));
        }
    }

    /// Applies what happened since the last call, leaving the damage to take.
    pub(crate) fn update(&mut self) {
        self.flush_gestures();
        while let Some((slot, msg, tap_rect)) = self.queue.pop_front() {
            self.dispatch(slot, msg, tap_rect);
        }
        while let Some(slot) = self.dirty.pop() {
            self.build_component(slot);
        }
        if self.sync_splits() {
            self.update();
            return;
        }
        if core::mem::take(&mut self.layout_dirty) {
            self.clear_measured_all();
            let area = self.area;
            self.arrange(self.root, area);
            self.resized = false;
        }
        for i in 0..self.lists.len() {
            self.sync_list(self.lists[i]);
        }
        self.sync_screen(self.root, 0, 0, self.area, false);
        if self.report_sizes() {
            self.update();
            return;
        }
        self.apply_redraws();
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
        let appearing = self.start_appearing();
        let animating = !self.pending_animations.is_empty();
        if animating {
            self.start_animations();
        }
        if appearing || animating {
            self.sync_screen(self.root, 0, 0, self.area, false);
        }
        self.release_waiting();
        self.sync_scrim();
    }

    fn sync_scrim(&mut self) {
        let alpha = self.scrim_alpha();
        let was = core::mem::replace(&mut self.scrim, alpha);
        if alpha != was {
            self.damage.push(self.nodes[self.overlay as usize].screen);
        }
    }

    pub(crate) fn scrim_color(&self) -> Color {
        self.theme.palette(self.color_mode).color(self.theme.widgets.overlay.scrim)
    }

    /// Follows how shown the overlay's screen is; 0 paints nothing.
    fn scrim_alpha(&self) -> u8 {
        let NodeContent::Navigator(navigator) = &self.nodes[self.overlay as usize].content else { return 0 };
        let screen = if navigator.departing != NONE { navigator.departing } else { navigator.shown() };
        let alpha = self.scrim_color().a;
        if screen == NONE || alpha == 0 {
            return 0;
        }
        (f32::from(alpha) * self.presence_level(screen)) as u8
    }

    /// Whether the node itself draws anything, apart from what is in it.
    fn paints(&self, id: NodeId) -> bool {
        let node = &self.nodes[id as usize];
        let style = &node.style;
        style.background.is_some()
            || node.fades.as_ref().is_some_and(|f| f.background.is_some())
            || (style.border_color.is_some() && style.border_width > 0)
            || style.border_top.is_some()
            || style.border_bottom.is_some()
            || style.scroll_border.is_some()
            || style.shadow.is_some()
            || node.pose.is_some()
            || matches!(node.content, NodeContent::Text(_) | NodeContent::Image { .. } | NodeContent::Canvas(_) | NodeContent::External(_) | NodeContent::Progress(_))
    }

    fn damage_extent(node: &Node) -> i16 {
        shadow_extent(&node.style) + node.pose.map_or(0, |p| p.reach)
    }

    /// Damages what `id` and what is in it drew, and nothing else.
    pub(crate) fn damage_painted(&mut self, id: NodeId) {
        let node = &self.nodes[id as usize];
        if !node.screen.is_empty() && self.paints(id) {
            self.damage.push(node.screen.expand(Self::damage_extent(node)));
        }
        if let NodeContent::List(list) = &self.nodes[id as usize].content {
            for (_, row) in list.rows.clone() {
                self.damage_painted(row);
            }
        }
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            self.damage_painted(child);
            child = self.nodes[child as usize].next;
        }
    }

    #[cfg(test)]
    pub(crate) fn dispatch_animation(&mut self, name: &'static str, animation: Animation) {
        self.pending_animations.push((name, animation));
    }

    fn start_animations(&mut self) {
        for (name, animation) in core::mem::take(&mut self.pending_animations) {
            match self.find(name) {
                Some(id) => self.start_animation(id, animation),
                None => idf::log::error!(c"ui", "animate: no node {name}"),
            }
        }
    }

    fn start_animation(&mut self, id: NodeId, animation: Animation) {
        self.stop_motions(id, |kind| matches!(kind, MotionKind::Value(a) if a.property == animation.property));
        self.motions.push(Motion::new(id, Some(Clock::once(u32::from(animation.duration_ms))), MotionKind::Value(animation)));
        self.apply_animation(self.motions.len() - 1);
    }

    /// Keeps the node where it shows, and lets it go to its new place. A node
    /// with no old place appears there.
    pub(crate) fn ease_position(&mut self, id: NodeId, old: Rect, new: Rect) {
        if old.is_empty() || self.resized {
            return;
        }
        let node = &self.nodes[id as usize];
        let (shift, duration_ms) = (node.translate, node.style.animate_ms);
        for (property, moved, shift) in [(Property::TranslateX, old.x - new.x, shift.0), (Property::TranslateY, old.y - new.y, shift.1)] {
            if moved != 0 {
                self.start_animation(id, Animation { property, from: moved + shift, to: 0, duration_ms });
            }
        }
    }

    pub(crate) fn apply_animation(&mut self, i: usize) {
        let motion = &self.motions[i];
        let (MotionKind::Value(animation), Some(clock)) = (&motion.kind, motion.clock) else { return };
        let range = f32::from(animation.to) - f32::from(animation.from);
        let value = animation.from + (range * clock.eased()) as i16;
        let node = &mut self.nodes[motion.node as usize];
        match animation.property {
            Property::TranslateX => node.translate.0 = value,
            Property::TranslateY => node.translate.1 = value,
            Property::Opacity => node.opacity = Some(value.clamp(0, 255) as u8),
        }
        node.paint_dirty = true;
    }

    pub(crate) fn end_animation(&mut self, id: NodeId, animation: &Animation) {
        if animation.property == Property::Opacity {
            self.nodes[id as usize].opacity = (animation.to < 255).then_some(animation.to.clamp(0, 255) as u8);
        }
    }

    /// The shown surface and its rectangle, kept to the row grid.
    pub(crate) fn external(&self) -> Option<(&ExternalSurface, Option<Rect>)> {
        if self.external == NONE {
            return None;
        }
        let node = &self.nodes[self.external as usize];
        let NodeContent::External(surface) = &node.content else { return None };
        let screen = node.screen.intersect(&self.area);
        let top = (screen.y + ROW_GRID - 1).div_euclid(ROW_GRID) * ROW_GRID;
        let bottom = screen.bottom().div_euclid(ROW_GRID) * ROW_GRID;
        let rect = Rect::new(screen.x, top, screen.w, bottom - top);
        Some((surface, (!rect.is_empty()).then_some(rect)))
    }

    fn report_sizes(&mut self) -> bool {
        let mut reported = false;
        for &id in &self.canvases {
            let node = &mut self.nodes[id as usize];
            let [left, top, right, bottom] = node.style.padding;
            let size = (node.rect.w - left - right, node.rect.h - top - bottom);
            let NodeContent::Canvas(canvas) = &mut node.content else { continue };
            if canvas.reported == size {
                continue;
            }
            canvas.reported = size;
            if let Some(map) = node.size {
                let msg = unsafe { (map.call)(map.function, size.0, size.1) };
                self.queue.push_back((node.owner, msg, None));
                reported = true;
            }
        }
        reported
    }

    fn apply_redraws(&mut self) {
        for (name, rect) in core::mem::take(&mut self.pending_redraw) {
            let Some(id) = self.find(name) else {
                idf::log::error!(c"ui", "redraw: no node {name}");
                continue;
            };
            let node = &self.nodes[id as usize];
            let [left, top, _, _] = node.style.padding;
            let screen = node.screen;
            self.damage.push(rect.map_or(screen, |r| r.offset(screen.x + left, screen.y + top).intersect(&screen)));
        }
    }

    pub(crate) fn caret_visible(&self, id: NodeId) -> bool {
        self.focus == id && self.caret_on
    }

    pub(crate) fn has_caret(&self) -> bool {
        self.focus != NONE && matches!(&self.nodes[self.focus as usize].content, NodeContent::Text(text) if text.edit.is_some())
    }

    fn set_focus(&mut self, id: NodeId) {
        if self.focus == id {
            return;
        }
        if self.focus != NONE {
            self.set_state(self.focus, State::FOCUSED, false);
            self.nodes[self.focus as usize].paint_dirty = true;
        }
        self.focus = id;
        if id != NONE {
            self.set_state(id, State::FOCUSED, true);
            self.nodes[id as usize].paint_dirty = true;
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
        self.queue.push_back((node.owner, msg, None));
        self.caret_on = true;
        self.caret_ms = 0;
    }

    /// Adds the docked on-screen keyboard, under only the popups.
    pub(crate) fn add_keyboard(&mut self, mode: KeyboardMode) {
        let slot = self.alloc_slot();
        let node = self.alloc_node(Node::new(NodeContent::Child(slot), slot, self.caps));
        let index = usize::from(slot);
        self.slots[index].host = Some(Box::new(Host::<KeyboardPanel>::new(KeyboardPanel::docked(), None)));
        self.slots[index].node = node;
        self.slots[index].theme = self.theme;
        self.keyboard_slot = Some(slot);
        self.keyboard_mode = mode;
        self.nodes[node as usize].style.absolute = Some(crate::style::Inset { left: Some(0), top: Some(0), right: Some(0), bottom: Some(0) });
        self.nodes[node as usize].parent = self.root;
        let mut child = self.nodes[self.root as usize].first_child;
        while self.nodes[child as usize].next != self.popups {
            child = self.nodes[child as usize].next;
        }
        self.nodes[child as usize].next = node;
        self.nodes[node as usize].next = self.popups;
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
        let height = layout.map_or(0, |l| l.height);
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
        let paints = self.paints(id);
        let node = &mut self.nodes[id as usize];
        let (shift_x, shift_y) = node.shift();
        let x = dx + i32::from(node.rect.x) + i32::from(shift_x);
        let y = dy + i32::from(node.rect.y) + i32::from(shift_y);
        let visible = y < i32::from(clip.bottom()) && y + i32::from(node.rect.h) > i32::from(clip.y);
        let screen = if visible { Rect::new(x as i16, y as i16, node.rect.w, node.rect.h) } else { Rect::default() };
        let quiet = quiet || core::mem::take(&mut node.scrolled);
        let moved = node.screen != screen;
        // Nothing of its own shows where it is placed; what is in it damages itself.
        let unseen = node.screen.is_empty() && !paints;
        if !quiet && !unseen && (moved || node.paint_dirty) {
            let extent = Self::damage_extent(node);
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
            && self.motions.is_empty()
            && self.presence_pending.is_empty()
            && !self.keyboard_changed
            && self.contacts.is_empty()
            && self.trackers.is_empty()
            && self.damage.is_empty()
            && self.pending_redraw.is_empty()
            && !self.layout_dirty
    }

    /// Something happens after a delay, without an event to wake for.
    pub(crate) fn timed(&self) -> bool {
        self.contacts.iter().any(|c| self.nodes.get(c.tap as usize).is_some_and(|n| n.repeat)) || self.gesture_timed()
    }

    fn set_offset(&mut self, id: NodeId, offset: i32) {
        let node = &mut self.nodes[id as usize];
        let Some(at) = node.scroll_offset_mut() else { return };
        let before = core::mem::replace(at, offset);
        if offset != before {
            node.scrolled = true;
            let screen = node.screen;
            self.damage.push(screen);
            self.scroll_moved(id, before);
        }
    }

    fn bounds(&self, id: NodeId) -> Bounds {
        let node = &self.nodes[id as usize];
        let span = i32::from(node.rect.h);
        let content = match &node.content {
            NodeContent::Scroll(s) => s.content,
            NodeContent::List(l) => l.count as i32 * i32::from(l.row_height),
            _ => 0,
        };
        let end = (content - span).max(0);
        let start = node.pull.filter(|p| p.refreshing).map_or(0, |p| -i32::from(p.height));
        let pull = node.pull.filter(|p| !p.refreshing).map_or(0, |p| p.height);
        let bounce = node.bounce && end > 0;
        Bounds { start: start as f32, end: end as f32, limit: span.max(1) as f32 * STRETCH_LIMIT, pull: f32::from(pull), past_start: bounce || node.pull.is_some(), past_end: bounce }
    }

    fn moving(&self, id: NodeId) -> bool {
        self.motion(id, |kind| matches!(kind, MotionKind::Fling(_))).is_some() || self.contacts.iter().any(|c| c.scroller == id)
    }

    /// Puts a scroller nothing moves back between its ends.
    pub(crate) fn keep_in_range(&mut self, id: NodeId) {
        if self.moving(id) {
            return;
        }
        let bounds = self.bounds(id);
        let Some(at) = self.nodes[id as usize].scroll_offset_mut() else { return };
        let before = *at;
        *at = (before as f32).clamp(bounds.start, bounds.end) as i32;
        if *at != before {
            self.scroll_moved(id, before);
        }
    }

    /// Springs a scroller nothing moves back if it is past an end.
    pub(crate) fn settle(&mut self, id: NodeId) {
        if id == NONE || self.moving(id) {
            return;
        }
        let Some(offset) = self.nodes[id as usize].scroll_offset() else { return };
        let position = offset as f32;
        if self.bounds(id).past(position).is_some() {
            self.fling(id, Fling { position, velocity: 0.0, anchor: None });
        }
    }

    fn let_go(&mut self, contact: &Contact, now_ms: u32) {
        let id = contact.scroller;
        let position = contact.position;
        let thrown = contact.dragging && now_ms.wrapping_sub(contact.last_ms) < FLING_STALE_MS;
        let mut velocity = if thrown { contact.velocity } else { 0.0 };
        let node = &mut self.nodes[id as usize];
        if let Some(pull) = &mut node.pull
            && !pull.refreshing
            && position <= -f32::from(pull.height)
        {
            pull.refreshing = true;
            let (owner, msg) = (node.owner, pull.msg);
            self.queue.push_back((owner, msg, None));
        }
        match self.bounds(id).past(position) {
            Some((edge, _)) => {
                if (position - edge) * velocity > 0.0 {
                    velocity = 0.0;
                }
                self.fling(id, Fling { position, velocity, anchor: None });
            }
            None if velocity.abs() > FLING_MIN => self.fling(id, Fling { position, velocity, anchor: None }),
            None => {}
        }
    }

    fn fling(&mut self, id: NodeId, fling: Fling) {
        self.motions.push(Motion::new(id, None, MotionKind::Fling(fling)));
    }

    /// Moves the fling at `i` on by `dt_ms`; `true` once it rests.
    pub(crate) fn advance(&mut self, i: usize, fling: Fling, dt_ms: u32) -> bool {
        let id = self.motions[i].node;
        let Fling { mut position, mut velocity, anchor } = fling;
        let bounds = self.bounds(id);
        let dt = (dt_ms as f32).min(MAX_FLING_STEP_MS);
        let rest = match anchor.or_else(|| bounds.past(position).map(|p| p.0)) {
            Some(target) => {
                let past = position - target;
                let push = velocity + SPRING_RATE * past;
                let fade = exp_neg(SPRING_RATE * dt);
                let next = (past + push * dt) * fade;
                velocity = (velocity - SPRING_RATE * push * dt) * fade;
                position = target + next;
                let crossed = anchor.is_none() && next * past < 0.0;
                let rest = if crossed { velocity.abs() <= FLING_MIN } else { next.abs() < SETTLE_PX && velocity.abs() < SETTLE_SPEED };
                if rest {
                    position = target;
                }
                rest
            }
            None => {
                position += velocity * dt_ms as f32;
                velocity *= 1.0 - FLING_DECAY * dt;
                match bounds.past(position) {
                    Some((edge, false)) => {
                        position = edge;
                        true
                    }
                    Some(_) => false,
                    None => velocity.abs() <= FLING_MIN,
                }
            }
        };
        self.motions[i].kind = MotionKind::Fling(Fling { position, velocity, anchor });
        self.set_offset(id, nearest(position));
        rest
    }

    fn sync_pull(&mut self, id: NodeId, pull: Option<Pull>) {
        let node = &mut self.nodes[id as usize];
        let was = node.pull.is_some_and(|p| p.refreshing);
        node.pull = pull;
        let Some(offset) = node.scroll_offset() else { return };
        match pull {
            Some(p) if p.refreshing && !was && offset <= 0 && offset > -i32::from(p.height) => {
                if self.contacts.iter().any(|c| c.scroller == id) {
                    return;
                }
                self.stop_motions(id, |kind| matches!(kind, MotionKind::Fling(_)));
                self.fling(id, Fling { position: offset as f32, velocity: 0.0, anchor: Some(-f32::from(p.height)) });
            }
            Some(Pull { refreshing: true, .. }) => {}
            _ if was => {
                for motion in self.motions.iter_mut().filter(|m| m.node == id) {
                    if let MotionKind::Fling(fling) = &mut motion.kind {
                        fling.anchor = None;
                    }
                }
                self.settle(id);
            }
            _ => {}
        }
    }

    /// Tells what follows `id`, whose offset has just left `before`.
    pub(crate) fn scroll_moved(&mut self, id: NodeId, before: i32) {
        let node = &self.nodes[id as usize];
        let offset = node.scroll_offset().unwrap_or(0);
        let (owner, name) = (node.owner, node.id);
        if let Some(map) = node.on_scroll {
            let msg = unsafe { (map.call)(map.function, offset) };
            self.queue.push_back((owner, msg, None));
        }
        let Some(name) = name.filter(|_| (before > 0) != (offset > 0)) else { return };
        let mut bars = Vec::new();
        self.walk(self.root, &mut |_, node| {
            if node.owner == owner && node.style.scroll_border == Some(name) {
                bars.push(node.screen);
            }
        });
        self.damage.extend(bars);
    }

    /// Whether the scroller `name` of `owner` is away from its start.
    pub(crate) fn scrolled_away(&self, owner: u16, name: &str) -> bool {
        let mut away = false;
        self.walk(self.root, &mut |_, node| {
            away |= node.owner == owner && node.id == Some(name) && node.scroll_offset().is_some_and(|offset| offset > 0);
        });
        away
    }

    fn hit(&self, id: NodeId, dx: i32, dy: i32, x: i32, y: i32) -> Option<Hit> {
        let node = &self.nodes[id as usize];
        if node.leaving {
            return None;
        }
        let (shift_x, shift_y) = node.shift();
        let left = dx + i32::from(node.rect.x) + i32::from(shift_x);
        let top = dy + i32::from(node.rect.y) + i32::from(shift_y);
        let right = left + i32::from(node.rect.w);
        let bottom = top + i32::from(node.rect.h);
        let inside = x >= left && x < right && y >= top && y < bottom;
        let clips = node.style.clip || matches!(node.content, NodeContent::Scroll(_) | NodeContent::List(_));
        if !inside && clips {
            return None;
        }
        if inside && node.state.contains(State::DISABLED) {
            return Some(NO_HIT);
        }
        if matches!(&node.content, NodeContent::Navigator(_)) && self.transit(id).is_some() {
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
                let panned = hit.gesture != NONE && self.gestures_of(hit.gesture).contains(Gestures::PAN);
                if hit.scroller == NONE && scrollable && !panned {
                    hit.scroller = id;
                }
            } else if node.drag.is_some() {
                found = Some(Hit { drag: id, ..NO_HIT });
            } else if node.tap.is_some() || node.input.is_some() || node.focusable {
                found = Some(Hit { tap: id, scroller: if scrollable { id } else { NONE }, ..NO_HIT });
            } else if scrollable {
                found = Some(Hit { scroller: id, ..NO_HIT });
            }
            if node.gestures.is_some() {
                let hit = found.get_or_insert(NO_HIT);
                if hit.gesture == NONE {
                    hit.gesture = id;
                }
            }
        }
        if found.is_none() && self.covers(id) {
            found = Some(Hit { tap: id, ..NO_HIT });
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

    pub(crate) fn set_pressed(&mut self, id: NodeId, pressed: bool) {
        if id != NONE {
            self.set_state(id, State::PRESSED, pressed);
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
        let hit = hit.unwrap_or(NO_HIT);
        let mut gesture = hit.gesture;
        let contact = Contact {
            track,
            passive: false,
            start: p,
            last: p,
            last_ms: now_ms,
            tap: NONE,
            scroller: NONE,
            drag: NONE,
            gesture,
            pan: hit.scroller == NONE && hit.drag == NONE,
            dragging: false,
            position: 0.0,
            velocity: 0.0,
            held_ms: 0,
            repeated: false,
        };
        if gesture != NONE && self.join_gesture(gesture, track, p) {
            self.contacts.push(Contact { passive: true, ..contact });
            self.gesture_joined(gesture);
            return;
        }
        if gesture != NONE && self.node_holds_fingers(gesture) {
            gesture = NONE;
        }
        let held = |id: NodeId, of: fn(&Contact) -> NodeId| id != NONE && self.contacts.iter().any(|c| of(c) == id);
        let passive = held(hit.scroller, |c| c.scroller) || held(hit.tap, |c| c.tap) || held(hit.drag, |c| c.drag);
        let hit = if passive { NO_HIT } else { hit };
        let gesture = if passive { NONE } else { gesture };
        self.stop_motions(hit.scroller, |kind| matches!(kind, MotionKind::Fling(_)));
        let position = self.nodes.get(hit.scroller as usize).and_then(|n| n.scroll_offset()).unwrap_or(0) as f32;
        self.set_pressed(hit.tap, true);
        self.set_pressed(hit.drag, true);
        self.send_drag(hit.drag, p, false);
        self.gesture_pressed(gesture, track, p, now_ms);
        self.contacts.push(Contact { passive, tap: hit.tap, scroller: hit.scroller, drag: hit.drag, gesture, position, ..contact });
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
            contact.position = self.bounds(contact.scroller).drag(contact.position, delta as f32);
            self.set_offset(contact.scroller, nearest(contact.position));
            let dt = now_ms.wrapping_sub(contact.last_ms).max(1) as f32;
            let speed = delta as f32 / dt;
            let weight = if starting { 1.0 } else { dt / (dt + VELOCITY_TAU_MS) };
            contact.velocity += (speed - contact.velocity) * weight;
            contact.last_ms = now_ms;
        }
        contact.last = p;
        self.contacts[index] = contact;
        self.gesture_moved(index);
    }

    fn release(&mut self, contact: Contact, now_ms: u32) {
        self.set_pressed(contact.tap, false);
        self.set_pressed(contact.drag, false);
        self.send_drag(contact.drag, contact.last, true);
        let consumed = self.gesture_released(&contact, now_ms);
        if contact.scroller != NONE {
            self.let_go(&contact, now_ms);
        }
        if contact.dragging || consumed {
        } else if contact.tap != NONE {
            self.finish_tap(contact.tap, contact.last, contact.repeated);
        } else if contact.drag == NONE && !contact.passive {
            self.set_focus(NONE);
        }
    }

    pub(crate) fn finish_tap(&mut self, id: NodeId, point: (i16, i16), repeated: bool) {
        if id == self.overlay {
            self.tap_scrim();
            return;
        }
        if id == self.popups {
            self.close_last_popup();
            return;
        }
        let node = &self.nodes[id as usize];
        let (tap, owner, rect) = (node.tap, node.owner, node.screen);
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
            self.queue.push_back((owner, msg, Some(rect)));
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
                    self.queue.push_back((node.owner, msg, Some(node.screen)));
                    contact.repeated = true;
                }
            }
        }
    }

    fn tick_caret(&mut self, dt_ms: u32) {
        if !self.has_caret() {
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
        let [left, top, right, _] = node.style.padding;
        let drag = Drag { x: point.0 - screen.x - left, y: point.1 - screen.y - top, width: screen.w - left - right, ended };
        let msg = unsafe { (map.call)(map.function, drag) };
        self.queue.push_back((node.owner, msg, None));
    }

    /// Advances what moves by itself.
    pub(crate) fn tick(&mut self, dt_ms: u32) {
        self.tick_motions(dt_ms);
        self.finish_departures();
        self.tick_repeat(dt_ms);
        self.tick_gestures(dt_ms);
        self.tick_caret(dt_ms);
    }

    pub(crate) fn walk(&self, id: NodeId, f: &mut impl FnMut(NodeId, &Node)) {
        let node = &self.nodes[id as usize];
        if node.screen == Rect::default() && !matches!(node.content, NodeContent::Child(_)) {
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
        self.queue.push_back((node.owner, msg, Some(node.screen)));
        Ok(())
    }

}
