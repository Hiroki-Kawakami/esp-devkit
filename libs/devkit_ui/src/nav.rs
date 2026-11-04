/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::TypeId;

use crate::app::Link;
use crate::component::{AnyHost, Requests, Screen, screen_host};
use crate::erased::Erased;
use crate::id::Id;
use crate::node::{NONE, Node, NodeContent, NodeIndex};
use crate::presenter::Strips;
use devkit_gfx::Rect;
use crate::style::{Dimension, Style};
use crate::motion::{Clock, Motion, MotionKind};
use crate::theme::{Theme, Transition};
use crate::tree::Tree;
use crate::zoom::Zoom;

/// The whole-screen navigator `App` owns.
pub struct Root;

/// A navigator above the chrome, over a scrim that keeps touches from what is
/// below. Holds one screen: a push replaces what shows at once, and a pop
/// empties it once the screen's `.disappear` ends.
pub struct Overlay;

/// A list's scroll position and a nested navigator, kept while hidden.
#[derive(Default)]
pub(crate) struct Saved {
    pub(crate) collections: Vec<(Id, i32, bool)>,
    pub(crate) navigators: Vec<(TypeId, Vec<Entry>)>,
}

/// A screen in a stack. Only the shown one has nodes.
pub(crate) struct Entry {
    pub(crate) host: Option<Box<dyn AnyHost>>,
    pub(crate) node: NodeIndex,
    pub(crate) saved: Saved,
    /// Asked for when it came, and played backwards when it goes.
    pub(crate) transition: Option<Transition>,
}

impl Entry {
    fn shown(node: NodeIndex, transition: Option<Transition>) -> Self {
        Self { host: None, node, saved: Saved::default(), transition }
    }
}

pub(crate) struct Transit {
    pub(crate) kind: Transition,
    pub(crate) leaving: NodeIndex,
    /// The leaving screen is gone for good, as after `pop`.
    pub(crate) drop_leaving: bool,
    pub(crate) forward: bool,
    /// Pictures of a zoom, taken before its first frame.
    pub(crate) zoom: Option<Box<Zoom>>,
    /// Taking the pictures held up the frame; that time is not part of it.
    pub(crate) skip_tick: bool,
    /// Where the panel may not show the leaving screen as it is.
    pub(crate) stale: Vec<Rect>,
}

/// What a screen is to the split navigator around it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SplitRole {
    /// Not split, or not in a navigator.
    #[default]
    None,
    /// The bottom of the stack, on the left. `detail` when something was opened
    /// on the right, which a default detail screen is not.
    Main { detail: bool },
    /// On the right.
    Detail,
}

/// Makes a default detail screen, without any state from the view.
#[derive(Clone, Copy)]
pub(crate) struct DetailMap {
    pub(crate) function: *const (),
    pub(crate) call: unsafe fn(*const ()) -> Box<dyn AnyHost>,
}

pub(crate) unsafe fn call_detail_map<S: Screen>(function: *const ()) -> Box<dyn AnyHost> {
    let function: fn() -> S = unsafe { core::mem::transmute(function) };
    screen_host(function())
}

/// What a view asks of a navigator.
#[derive(Clone, Copy, Default)]
pub(crate) struct SplitSpec {
    pub(crate) width: Option<i16>,
    pub(crate) detail: Option<DetailMap>,
}

pub(crate) struct NavigatorNode {
    pub(crate) key: TypeId,
    pub(crate) stack: Vec<Entry>,
    pub(crate) transition: Option<Transition>,
    /// Width of the main screen while split.
    pub(crate) split: Option<i16>,
    pub(crate) wanted: SplitSpec,
    /// The default detail screen, outside the stack until something is pushed
    /// from it, so a turn to one pane does not show it.
    pub(crate) spare: NodeIndex,
    /// The screen above the main one started as the default detail.
    pub(crate) promoted: bool,
    /// Popped, and kept until what is in it disappears.
    pub(crate) departing: NodeIndex,
}

impl NavigatorNode {
    pub(crate) fn new(key: TypeId) -> Self {
        Self { key, stack: Vec::new(), transition: None, split: None, wanted: SplitSpec::default(), spare: NONE, promoted: false, departing: NONE }
    }

    pub(crate) fn main(&self) -> NodeIndex {
        self.stack.first().map_or(NONE, |e| e.node)
    }

    /// The screen a transition moves: the one on the right while split.
    pub(crate) fn shown(&self) -> NodeIndex {
        if self.split.is_some() && self.stack.len() == 1 {
            return self.spare;
        }
        self.stack.last().map_or(NONE, |e| e.node)
    }

    fn opened(&self) -> bool {
        self.stack.len() > 1 && !(self.promoted && self.stack.len() == 2)
    }
}

pub(crate) enum Action {
    Push(Box<dyn AnyHost>),
    Pop,
    Replace(Box<dyn AnyHost>),
    Show(Box<dyn AnyHost>),
    Root,
}

pub(crate) struct NavOp {
    pub(crate) target: Option<TypeId>,
    pub(crate) action: Action,
    pub(crate) transition: Option<Transition>,
}

/// Finds `id` in `stack` and in what its screens keep of the navigators inside
/// them. Those have no nodes, so the only move left to them is back to the root.
fn deliver(stack: &mut [Entry], id: u32, msg: Erased, replaced: &mut Option<Strips>, link: &Link) -> Option<Requests> {
    for entry in stack {
        if let Some(host) = &mut entry.host
            && host.id() == id
        {
            return Some(host.dispatch(msg, replaced.take(), None, link));
        }
        for (key, nested) in &mut entry.saved.navigators {
            if let Some(mut requests) = deliver(nested, id, msg, replaced, link) {
                for op in core::mem::take(&mut requests.nav) {
                    if matches!(op.action, Action::Root) && op.target.is_none_or(|target| target == *key) {
                        nested.truncate(1);
                    } else {
                        idf::log::error!(c"ui", "a navigator inside a covered screen can only go to its root");
                    }
                }
                return Some(requests);
            }
        }
    }
    None
}

impl Tree {
    fn navigator(&mut self, id: NodeIndex) -> Option<&mut NavigatorNode> {
        match &mut self.nodes[id as usize].content {
            NodeContent::Navigator(nav) => Some(nav),
            _ => None,
        }
    }

    fn find_below(&self, id: NodeIndex, target: TypeId) -> Option<NodeIndex> {
        if let NodeContent::Navigator(nav) = &self.nodes[id as usize].content
            && nav.key == target
        {
            return Some(id);
        }
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            if let Some(found) = self.find_below(child, target) {
                return Some(found);
            }
            child = self.nodes[child as usize].next;
        }
        None
    }

    /// Nearest above `from`, then below it, then anywhere. Without a target: the nearest that
    /// can pop for a pop, the nearest of any kind otherwise.
    fn resolve(&self, from: NodeIndex, target: Option<TypeId>, popping: bool) -> Option<NodeIndex> {
        let mut at = from;
        while at != NONE {
            let node = &self.nodes[at as usize];
            if let NodeContent::Navigator(nav) = &node.content {
                let wanted = match target {
                    Some(key) => nav.key == key,
                    None => !popping || nav.stack.len() > 1,
                };
                if wanted {
                    return Some(at);
                }
            }
            at = node.parent;
        }
        target.and_then(|key| self.find_below(from, key).or_else(|| self.find_below(self.root, key)))
    }

    pub(crate) fn apply_nav(&mut self, from: u16, op: NavOp) {
        self.apply_nav_from(self.slots[usize::from(from)].node, op);
    }

    fn apply_nav_from(&mut self, node: NodeIndex, op: NavOp) {
        let popping = matches!(op.action, Action::Pop);
        let Some(nav) = self.resolve(node, op.target, popping) else {
            idf::log::error!(c"ui", "no navigator for this operation");
            return;
        };
        if nav == self.overlay && !popping {
            self.clear_overlay(nav);
        }
        let pane = self.nodes[nav as usize].screen;
        let stale: Vec<Rect> = if self.finish_transit(nav) { Vec::from([pane]) } else { self.damage.iter().filter(|r| r.intersects(&pane)).copied().collect() };
        let asked = op.transition;
        match op.action {
            Action::Push(host) => self.push_screen(nav, host, asked),
            Action::Pop => self.pop_screen(nav, asked),
            Action::Replace(host) => self.replace_screen(nav, host, asked),
            Action::Show(host) => self.show_screen(nav, host, asked),
            Action::Root => self.pop_to_root(nav, asked),
        }
        if let Some(transit) = self.transit_mut(nav) {
            transit.stale = stale;
        }
        self.style_changed(nav);
        self.refresh_shown(nav);
    }

    /// Their `ui.can_pop()` and `ui.split_role()` may have changed. A new screen
    /// needs this too: it is built before it joins the stack.
    fn refresh_shown(&mut self, nav: NodeIndex) {
        let Some(navigator) = self.navigator(nav) else { return };
        let (main, shown) = (navigator.main(), navigator.shown());
        for node in [main, shown] {
            if node != NONE
                && let NodeContent::Child(slot) = self.nodes[node as usize].content
            {
                self.mark_slot_dirty(slot);
            }
        }
    }

    /// Where the screen of `slot` stands in its navigator: `(role, can pop)`.
    pub(crate) fn nav_state(&self, slot: u16) -> (SplitRole, bool) {
        let mut at = self.slots[usize::from(slot)].node;
        while at != NONE {
            let parent = self.nodes[at as usize].parent;
            if parent == NONE {
                break;
            }
            if let NodeContent::Navigator(nav) = &self.nodes[parent as usize].content {
                if nav.split.is_none() {
                    return (SplitRole::None, nav.stack.len() > 1);
                }
                if nav.main() == at {
                    return (SplitRole::Main { detail: nav.opened() }, false);
                }
                return (SplitRole::Detail, nav.stack.len() > 2);
            }
            at = parent;
        }
        (SplitRole::None, false)
    }

    /// Applies what views asked of split navigators. `true` when screens came or
    /// went, which their views must see.
    pub(crate) fn sync_splits(&mut self) -> bool {
        let mut changed = false;
        for nav in self.navigators.clone() {
            let NodeContent::Navigator(navigator) = &self.nodes[nav as usize].content else { continue };
            let (width, was) = (navigator.wanted.width, navigator.split);
            if width == was {
                continue;
            }
            self.finish_transit(nav);
            if let Some(navigator) = self.navigator(nav) {
                navigator.split = width;
            }
            match (was, width) {
                (None, Some(_)) => self.open_split(nav),
                (Some(_), None) => self.close_split(nav),
                _ => {}
            }
            self.layout_dirty = true;
            self.refresh_shown(nav);
            changed = true;
        }
        changed
    }

    fn open_split(&mut self, nav: NodeIndex) {
        let Some(navigator) = self.navigator(nav) else { return };
        if navigator.stack.len() == 1 {
            self.show_spare(nav);
            return;
        }
        let Some(first) = navigator.stack.first_mut() else { return };
        let Some(host) = first.host.take() else { return };
        let saved = core::mem::take(&mut first.saved);
        let node = self.mount(nav, host, saved);
        if let Some(first) = self.navigator(nav).and_then(|n| n.stack.first_mut()) {
            first.node = node;
        }
    }

    fn close_split(&mut self, nav: NodeIndex) {
        self.drop_spare(nav);
        let Some(navigator) = self.navigator(nav) else { return };
        if navigator.stack.len() > 1 {
            let main = navigator.main();
            self.disappear_begin_within(main);
            self.retire(nav, main, false);
        }
    }

    fn show_spare(&mut self, nav: NodeIndex) -> NodeIndex {
        let Some(detail) = self.navigator(nav).and_then(|n| n.wanted.detail) else { return NONE };
        let host = unsafe { (detail.call)(detail.function) };
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.spare = node;
        }
        node
    }

    fn drop_spare(&mut self, nav: NodeIndex) {
        let Some(navigator) = self.navigator(nav) else { return };
        let spare = core::mem::replace(&mut navigator.spare, NONE);
        if spare != NONE {
            self.unmount(nav, spare);
        }
    }

    /// Runs `msg` on the component `id`, shown or covered. Covered, it has no
    /// nodes to rebuild or animate: its next view shows what the message did.
    pub(crate) fn post(&mut self, id: u32, msg: Erased) {
        if let Some(slot) = self.slot_of(id) {
            self.dispatch(slot, msg, None);
            return;
        }
        let mut replaced = self.parked_strips.take();
        let link = self.link.clone();
        for nav in self.navigators.clone() {
            let Some(navigator) = self.navigator(nav) else { continue };
            let Some(requests) = deliver(&mut navigator.stack, id, msg, &mut replaced, &link) else { continue };
            self.parked_strips = replaced;
            self.apply_covered(id, nav, requests);
            return;
        }
        self.parked_strips = replaced;
    }

    fn apply_covered(&mut self, id: u32, nav: NodeIndex, requests: Requests) {
        if let Some(theme) = requests.theme {
            self.set_theme(theme);
        }
        if requests.render.is_some() {
            idf::log::error!(c"ui", "a covered screen cannot change the render mode");
        }
        if !requests.popups.is_empty() {
            idf::log::error!(c"ui", "a covered screen cannot open a popup");
        }
        if !requests.guards.is_empty() {
            idf::log::error!(c"ui", "while_shown: the screen is not shown");
        }
        for op in requests.nav {
            self.apply_nav_from(nav, op);
        }
        for msg in requests.sends {
            self.post(id, msg);
        }
        for (id, msg) in requests.posts {
            self.post(id, msg);
        }
    }

    fn disappear_begin_node(&mut self, node: NodeIndex) {
        if node != NONE {
            self.disappear_begin_within(node);
        }
    }

    fn top(&mut self, nav: NodeIndex) -> NodeIndex {
        self.navigator(nav).and_then(|n| n.stack.last()).map_or(NONE, |e| e.node)
    }

    fn fill_style() -> Style {
        Style { width: Dimension::Auto, height: Dimension::Auto, ..Style::default() }
    }

    /// Creates the nodes of a screen, as a new last child of `nav`.
    pub(crate) fn mount(&mut self, nav: NodeIndex, host: Box<dyn AnyHost>, saved: Saved) -> NodeIndex {
        let parent_slot = self.nodes[nav as usize].owner;
        let inherited = self.slots.get(usize::from(parent_slot)).map_or(self.theme, |s| s.theme);
        self.mount_themed(nav, host, saved, inherited, true)
    }

    fn mount_themed(&mut self, nav: NodeIndex, host: Box<dyn AnyHost>, saved: Saved, inherited: &'static Theme, screen: bool) -> NodeIndex {
        let theme = host.theme();
        let slot = self.alloc_slot();
        let node = self.alloc_node(Node::new(NodeContent::Child(slot), slot, self.caps));
        self.nodes[node as usize].style = Self::fill_style();
        self.nodes[node as usize].parent = nav;
        let slot_index = usize::from(slot);
        self.slots[slot_index].host = Some(host);
        self.slots[slot_index].node = node;
        self.slots[slot_index].theme = theme.unwrap_or(inherited);
        self.link_last(nav, node);
        if screen {
            self.appear_begin(slot);
        }
        self.restoring.push(saved);
        self.build_component(slot);
        self.restoring.pop();
        self.layout_dirty = true;
        node
    }

    /// Shows `host` above everything, in the theme of the component `from`.
    pub(crate) fn open_popup(&mut self, from: u16, host: Box<dyn AnyHost>) {
        let theme = self.slots[usize::from(from)].theme;
        let node = self.mount_themed(self.popups, host, Saved::default(), theme, false);
        self.nodes[node as usize].style.absolute = Some(crate::style::Inset { left: Some(0), top: Some(0), right: Some(0), bottom: Some(0) });
    }

    /// Closes the popup that component `slot` is part of.
    pub(crate) fn close_popup(&mut self, slot: u16) {
        let mut node = self.slots[usize::from(slot)].node;
        while node != NONE && self.nodes[node as usize].parent != self.popups {
            node = self.nodes[node as usize].parent;
        }
        if node == NONE {
            idf::log::error!(c"ui", "close_popup outside a popup");
            return;
        }
        self.unmount(self.popups, node);
    }

    pub(crate) fn close_last_popup(&mut self) {
        let mut popup = self.nodes[self.popups as usize].first_child;
        while popup != NONE && self.nodes[popup as usize].next != NONE {
            popup = self.nodes[popup as usize].next;
        }
        if popup != NONE {
            self.unmount(self.popups, popup);
        }
    }

    /// Whether `id` takes the touches nothing in it takes.
    pub(crate) fn covers(&self, id: NodeIndex) -> bool {
        if id == self.popups {
            return self.nodes[id as usize].first_child != NONE;
        }
        id == self.overlay && matches!(&self.nodes[id as usize].content, NodeContent::Navigator(n) if !n.stack.is_empty() || n.departing != NONE)
    }

    pub(crate) fn tap_scrim(&mut self) {
        let NodeContent::Navigator(navigator) = &self.nodes[self.overlay as usize].content else { return };
        let shown = navigator.shown();
        if shown == NONE {
            return;
        }
        let NodeContent::Child(slot) = self.nodes[shown as usize].content else { return };
        if let Some(msg) = self.slots[usize::from(slot)].host.as_ref().and_then(|host| host.scrim_tap()) {
            self.queue.push_back((slot, msg, None));
        }
    }

    /// Takes everything off the overlay at once.
    fn clear_overlay(&mut self, nav: NodeIndex) {
        let Some(navigator) = self.navigator(nav) else { return };
        let departing = core::mem::replace(&mut navigator.departing, NONE);
        let stack = core::mem::take(&mut navigator.stack);
        if departing != NONE {
            self.retire(nav, departing, true);
        }
        for entry in stack.into_iter().filter(|e| e.node != NONE) {
            self.unmount(nav, entry.node);
        }
    }

    /// Removes a popped screen once what is in it has disappeared.
    fn depart(&mut self, nav: NodeIndex, leaving: NodeIndex) {
        if !self.disappear_within(leaving) {
            self.retire(nav, leaving, true);
            return;
        }
        self.nodes[leaving as usize].leaving = true;
        if let Some(navigator) = self.navigator(nav) {
            navigator.departing = leaving;
        }
    }

    pub(crate) fn finish_departures(&mut self) {
        let nav = self.overlay;
        let Some(departing) = self.navigator(nav).map(|n| n.departing) else { return };
        if departing == NONE || self.presence_within(departing) {
            return;
        }
        if let Some(navigator) = self.navigator(nav) {
            navigator.departing = NONE;
        }
        self.retire(nav, departing, true);
    }

    pub(crate) fn close_popups(&mut self) {
        loop {
            let popup = self.nodes[self.popups as usize].first_child;
            if popup == NONE {
                return;
            }
            self.unmount(self.popups, popup);
        }
    }

    fn link_last(&mut self, parent: NodeIndex, node: NodeIndex) {
        let mut child = self.nodes[parent as usize].first_child;
        if child == NONE {
            self.nodes[parent as usize].first_child = node;
            return;
        }
        while self.nodes[child as usize].next != NONE {
            child = self.nodes[child as usize].next;
        }
        self.nodes[child as usize].next = node;
    }

    pub(crate) fn unlink(&mut self, parent: NodeIndex, node: NodeIndex) {
        let next = self.nodes[node as usize].next;
        let mut child = self.nodes[parent as usize].first_child;
        if child == node {
            self.nodes[parent as usize].first_child = next;
        } else {
            while child != NONE && self.nodes[child as usize].next != node {
                child = self.nodes[child as usize].next;
            }
            if child != NONE {
                self.nodes[child as usize].next = next;
            }
        }
        self.nodes[node as usize].next = NONE;
    }

    fn collect(&mut self, id: NodeIndex, saved: &mut Saved) {
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            let next = self.nodes[child as usize].next;
            self.collect(child, saved);
            child = next;
        }
        let node = &mut self.nodes[id as usize];
        let pinned = node.pinned;
        match &mut node.content {
            NodeContent::Collection(list) => {
                if let Some(list_id) = node.id {
                    saved.collections.push((list_id, list.offset.max(0), pinned));
                }
            }
            NodeContent::Navigator(nav) => {
                let stack = core::mem::take(&mut nav.stack);
                let key = nav.key;
                saved.navigators.push((key, stack));
            }
            _ => {}
        }
    }

    /// Removes a screen's nodes, keeping what it needs to come back.
    fn unmount(&mut self, nav: NodeIndex, node: NodeIndex) -> (Option<Box<dyn AnyHost>>, Saved) {
        self.disappear_end_within(node);
        let mut saved = Saved::default();
        // Nested navigators' shown screens leave their nodes with this subtree.
        self.collect(node, &mut saved);
        for (_, stack) in &mut saved.navigators {
            // Both ends of a split stack are shown.
            for entry in stack.iter_mut().filter(|e| e.node != NONE) {
                entry.host = self.take_host(entry.node);
                entry.node = NONE;
            }
        }
        let host = self.take_host(node);
        self.damage_painted(node);
        self.unlink(nav, node);
        self.free_subtree(node);
        self.layout_dirty = true;
        (host, saved)
    }

    fn take_host(&mut self, node: NodeIndex) -> Option<Box<dyn AnyHost>> {
        match self.nodes[node as usize].content {
            NodeContent::Child(slot) => self.slots[usize::from(slot)].host.take(),
            _ => None,
        }
    }

    /// What a move shows: `asked`, then what the screen came with, then the
    /// navigator's and the theme's defaults. Without `defaulted`, only `asked`.
    fn pick(&self, nav: NodeIndex, asked: Option<Transition>, came_with: Option<Transition>, defaulted: bool) -> Transition {
        let NodeContent::Navigator(navigator) = &self.nodes[nav as usize].content else { return Transition::None };
        if navigator.key == TypeId::of::<Overlay>() {
            return Transition::None;
        }
        if !defaulted {
            return asked.unwrap_or(Transition::None);
        }
        asked.or(came_with).or(navigator.transition).unwrap_or(self.theme.motion.transition)
    }

    fn begin_transit(&mut self, nav: NodeIndex, leaving: NodeIndex, drop_leaving: bool, forward: bool, kind: Transition) {
        if leaving == NONE || kind == Transition::None {
            if leaving != NONE {
                self.retire(nav, leaving, drop_leaving);
            }
            return;
        }
        let clock = Clock::once(u32::from(self.theme.motion.duration_ms(kind)));
        let transit = Transit { kind, leaving, drop_leaving, forward, zoom: None, skip_tick: false, stale: Vec::new() };
        self.motions.push(Motion::new(nav, Some(clock), MotionKind::Transit(transit)));
        self.step_transit(self.motions.len() - 1);
    }

    fn retire(&mut self, nav: NodeIndex, leaving: NodeIndex, drop_leaving: bool) {
        let (host, saved) = self.unmount(nav, leaving);
        if drop_leaving {
            return;
        }
        let entry = self.navigator(nav).and_then(|n| n.stack.iter_mut().find(|e| e.node == leaving));
        if let Some(entry) = entry {
            entry.host = host;
            entry.node = NONE;
            entry.saved = saved;
        }
    }

    /// `false` when none was running.
    pub(crate) fn finish_transit(&mut self, nav: NodeIndex) -> bool {
        let Some(i) = self.motion(nav, |kind| matches!(kind, MotionKind::Transit(_))) else { return false };
        let MotionKind::Transit(transit) = self.take_motion(i).kind else { return false };
        self.end_transit(nav, transit);
        true
    }

    pub(crate) fn end_transit(&mut self, nav: NodeIndex, transit: Transit) {
        let top = self.navigator(nav).map_or(NONE, |n| n.shown());
        if top != NONE {
            self.nodes[top as usize].translate = (0, 0);
        }
        self.retire(nav, transit.leaving, transit.drop_leaving);
        self.mark_dirty_rect(nav);
    }

    fn mark_dirty_rect(&mut self, nav: NodeIndex) {
        let screen = self.nodes[nav as usize].screen;
        self.damage.push(screen);
        self.nodes[nav as usize].scrolled = true;
    }

    pub(crate) fn step_transit(&mut self, i: usize) {
        let nav = self.motions[i].node;
        let top = self.navigator(nav).map_or(NONE, |n| n.shown());
        let side = self.navigator(nav).and_then(|n| n.split).unwrap_or(0);
        let (width, height) = (self.nodes[nav as usize].rect.w - side, self.nodes[nav as usize].rect.h);
        let motion = &mut self.motions[i];
        let (MotionKind::Transit(transit), Some(clock)) = (&mut motion.kind, motion.clock) else { return };
        let (forward, leaving, kind) = (transit.forward, transit.leaving, transit.kind);
        let eased = clock.eased();
        if let Some(zoom) = &mut transit.zoom {
            zoom.set_progress(eased);
        }
        let (entering, departing) = match kind {
            Transition::Slide => {
                let shift = (f32::from(width) * eased) as i16;
                if forward { ((width - shift, 0), (-shift / 3, 0)) } else { ((-(width - shift) / 3, 0), (shift, 0)) }
            }
            Transition::SlideUp => {
                let shift = (f32::from(height) * eased) as i16;
                if forward { ((0, height - shift), (0, 0)) } else { ((0, 0), (0, shift)) }
            }
            Transition::None | Transition::Zoom => ((0, 0), (0, 0)),
        };
        self.nodes[top as usize].translate = entering;
        self.nodes[leaving as usize].translate = departing;
        self.mark_dirty_rect(nav);
    }

    fn push_screen(&mut self, nav: NodeIndex, host: Box<dyn AnyHost>, asked: Option<Transition>) {
        // Pushing from the default detail keeps it to come back to.
        if let Some(navigator) = self.navigator(nav)
            && navigator.spare != NONE
        {
            let node = core::mem::replace(&mut navigator.spare, NONE);
            navigator.stack.push(Entry::shown(node, None));
            navigator.promoted = true;
        }
        let leaving = self.navigator(nav).map_or(NONE, |n| n.shown());
        self.disappear_begin_node(leaving);
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry::shown(node, asked));
        }
        let kind = self.pick(nav, asked, None, true);
        self.begin_transit(nav, leaving, false, true, kind);
    }

    fn pop_screen(&mut self, nav: NodeIndex, asked: Option<Transition>) {
        let Some(navigator) = self.navigator(nav) else { return };
        let empties = navigator.key == TypeId::of::<Overlay>();
        if navigator.stack.len() < 2 && !empties {
            return;
        }
        let split = navigator.split.is_some();
        if split && !navigator.opened() {
            return;
        }
        let Some(entry) = navigator.stack.pop() else { return };
        if navigator.stack.len() < 2 {
            navigator.promoted = false;
        }
        let emptied = split && navigator.stack.len() == 1;
        let leaving = entry.node;
        self.disappear_begin_node(leaving);
        let top = if emptied { self.show_spare(nav) } else { self.show_top(nav) };
        if top == NONE {
            self.depart(nav, leaving);
            return;
        }
        // The leaving screen paints above the one coming back.
        self.unlink(nav, top);
        self.link_before(nav, top, leaving);
        let kind = self.pick(nav, asked, entry.transition, true);
        self.begin_transit(nav, leaving, true, false, kind);
    }

    fn link_before(&mut self, parent: NodeIndex, node: NodeIndex, before: NodeIndex) {
        self.nodes[node as usize].next = before;
        if self.nodes[parent as usize].first_child == before {
            self.nodes[parent as usize].first_child = node;
            return;
        }
        let mut child = self.nodes[parent as usize].first_child;
        while child != NONE && self.nodes[child as usize].next != before {
            child = self.nodes[child as usize].next;
        }
        if child != NONE {
            self.nodes[child as usize].next = node;
        }
    }

    /// Replaces everything above the bottom of the stack with `host`.
    fn show_screen(&mut self, nav: NodeIndex, host: Box<dyn AnyHost>, asked: Option<Transition>) {
        let Some(navigator) = self.navigator(nav) else { return };
        let (main, leaving, split) = (navigator.main(), navigator.shown(), navigator.split.is_some());
        let dropped = leaving != main;
        navigator.promoted = false;
        navigator.spare = NONE;
        navigator.stack.truncate(1);
        self.disappear_begin_node(leaving);
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry::shown(node, asked));
        }
        // Next to the main screen this changes what is shown, not where we are.
        let kind = self.pick(nav, asked, None, !split);
        self.begin_transit(nav, leaving, dropped || split, true, kind);
    }

    fn show_top(&mut self, nav: NodeIndex) -> NodeIndex {
        let Some(navigator) = self.navigator(nav) else { return NONE };
        let Some(entry) = navigator.stack.last_mut() else { return NONE };
        let Some(host) = entry.host.take() else { return entry.node };
        let saved = core::mem::take(&mut entry.saved);
        let node = self.mount(nav, host, saved);
        if let Some(entry) = self.navigator(nav).and_then(|n| n.stack.last_mut()) {
            entry.node = node;
        }
        node
    }

    fn replace_screen(&mut self, nav: NodeIndex, host: Box<dyn AnyHost>, asked: Option<Transition>) {
        let leaving = self.top(nav);
        self.disappear_begin_node(leaving);
        let Some(navigator) = self.navigator(nav) else { return };
        let main = navigator.split.is_some() && leaving == navigator.main();
        let came_with = navigator.stack.pop().and_then(|e| e.transition);
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry::shown(node, came_with));
        }
        // The main screen of a split navigator does not move.
        let kind = if main { Transition::None } else { self.pick(nav, asked, None, false) };
        self.begin_transit(nav, leaving, true, true, kind);
    }

    fn pop_to_root(&mut self, nav: NodeIndex, asked: Option<Transition>) {
        let Some(navigator) = self.navigator(nav) else { return };
        if navigator.stack.len() < 2 {
            return;
        }
        let shown = navigator.stack.len() - 1;
        navigator.stack.drain(1..shown);
        navigator.promoted = false;
        self.pop_screen(nav, asked);
    }

    /// Fills `area` with a navigator node owned by `owner`.
    pub(crate) fn new_navigator(&mut self, key: TypeId, owner: u16) -> NodeIndex {
        let content = NodeContent::Navigator(NavigatorNode::new(key));
        let id = self.alloc_node(Node::new(content, owner, self.caps));
        self.nodes[id as usize].style.clip = true;
        self.navigators.push(id);
        id
    }

    pub(crate) fn seed_navigator(&mut self, nav: NodeIndex, host: Box<dyn AnyHost>) {
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry::shown(node, None));
        }
    }

    /// Gives a navigator its hidden stack back, if the screen around it kept one.
    pub(crate) fn restore_navigator(&mut self, nav: NodeIndex, key: TypeId) -> bool {
        let Some(saved) = self.restoring.last_mut() else { return false };
        let Some(at) = saved.navigators.iter().position(|(k, _)| *k == key) else { return false };
        let (_, stack) = saved.navigators.swap_remove(at);
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack = stack;
        }
        self.show_top(nav) != NONE
    }

    fn style_changed(&mut self, nav: NodeIndex) {
        if nav != self.root_navigator {
            return;
        }
        let top = self.top(nav);
        let style = match self.nodes.get(top as usize).map(|n| &n.content) {
            Some(NodeContent::Child(slot)) => {
                self.slots[usize::from(*slot)].host.as_ref().and_then(|h| h.screen_style())
            }
            _ => None,
        };
        let style = style.unwrap_or_default();
        if style != self.screen_style {
            self.screen_style = style;
            for slot in self.chrome.clone() {
                self.mark_slot_dirty(slot);
            }
        }
    }
}
