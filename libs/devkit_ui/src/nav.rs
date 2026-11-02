/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::TypeId;

use crate::app::Link;
use crate::component::{AnyHost, Hook, Requests, Screen, screen_host};
use crate::erased::Erased;
use crate::node::{NONE, Node, NodeContent, NodeId};
use crate::presenter::Strips;
use devkit_gfx::Rect;
use crate::style::{Dimension, Style};
use crate::animate::ease_out;
use crate::theme::{Theme, Transition};
use crate::tree::Tree;
use crate::zoom::Zoom;

/// The whole-screen navigator `App` owns.
pub struct Root;

/// A navigator above the chrome, for modals. Empty unless something is shown.
pub struct Overlay;

/// A list's scroll position and a nested navigator, kept while hidden.
#[derive(Default)]
pub(crate) struct Saved {
    pub(crate) lists: Vec<(u16, i32)>,
    pub(crate) navigators: Vec<(TypeId, Vec<Entry>)>,
}

/// A screen in a stack. Only the shown one has nodes.
pub(crate) struct Entry {
    pub(crate) host: Option<Box<dyn AnyHost>>,
    pub(crate) node: NodeId,
    pub(crate) saved: Saved,
    /// Asked for when it came, and played backwards when it goes.
    pub(crate) transition: Option<Transition>,
}

impl Entry {
    fn shown(node: NodeId, transition: Option<Transition>) -> Self {
        Self { host: None, node, saved: Saved::default(), transition }
    }
}

pub(crate) struct Transit {
    pub(crate) kind: Transition,
    pub(crate) leaving: NodeId,
    /// The leaving screen is gone for good, as after `pop`.
    pub(crate) drop_leaving: bool,
    pub(crate) elapsed: u32,
    pub(crate) duration: u32,
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
    pub(crate) transit: Option<Transit>,
    pub(crate) transition: Option<Transition>,
    /// Width of the main screen while split.
    pub(crate) split: Option<i16>,
    pub(crate) wanted: SplitSpec,
    /// The default detail screen, outside the stack until something is pushed
    /// from it, so a turn to one pane does not show it.
    pub(crate) spare: NodeId,
    /// The screen above the main one started as the default detail.
    pub(crate) promoted: bool,
}

impl NavigatorNode {
    pub(crate) fn new(key: TypeId) -> Self {
        Self { key, stack: Vec::new(), transit: None, transition: None, split: None, wanted: SplitSpec::default(), spare: NONE, promoted: false }
    }

    pub(crate) fn main(&self) -> NodeId {
        self.stack.first().map_or(NONE, |e| e.node)
    }

    /// The screen a transition moves: the one on the right while split.
    pub(crate) fn shown(&self) -> NodeId {
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
                        drop_to_root(nested, link);
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

fn drop_to_root(stack: &mut Vec<Entry>, link: &Link) {
    for mut entry in stack.drain(1..) {
        if let Some(host) = &mut entry.host {
            host.hook(Hook::Exit, link);
        }
    }
}

impl Tree {
    fn navigator(&mut self, id: NodeId) -> Option<&mut NavigatorNode> {
        match &mut self.nodes[id as usize].content {
            NodeContent::Navigator(nav) => Some(nav),
            _ => None,
        }
    }

    fn find_below(&self, id: NodeId, target: TypeId) -> Option<NodeId> {
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
    fn resolve(&self, from: NodeId, target: Option<TypeId>, popping: bool) -> Option<NodeId> {
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

    fn apply_nav_from(&mut self, node: NodeId, op: NavOp) {
        let popping = matches!(op.action, Action::Pop);
        let Some(nav) = self.resolve(node, op.target, popping) else {
            idf::log::error!(c"ui", "no navigator for this operation");
            return;
        };
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
        if let Some(transit) = self.navigator(nav).and_then(|n| n.transit.as_mut()) {
            transit.stale = stale;
        }
        self.style_changed(nav);
        self.refresh_shown(nav);
    }

    /// Their `ui.can_pop()` and `ui.split_role()` may have changed. A new screen
    /// needs this too: it is built before it joins the stack.
    fn refresh_shown(&mut self, nav: NodeId) {
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

    fn open_split(&mut self, nav: NodeId) {
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
        self.hook(node, Hook::Appear);
    }

    fn close_split(&mut self, nav: NodeId) {
        self.drop_spare(nav);
        let Some(navigator) = self.navigator(nav) else { return };
        if navigator.stack.len() > 1 {
            let main = navigator.main();
            self.hook(main, Hook::Disappear);
            self.retire(nav, main, false);
        }
    }

    fn show_spare(&mut self, nav: NodeId) -> NodeId {
        let Some(detail) = self.navigator(nav).and_then(|n| n.wanted.detail) else { return NONE };
        let host = unsafe { (detail.call)(detail.function) };
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.spare = node;
        }
        self.hook(node, Hook::Enter);
        self.hook(node, Hook::Appear);
        node
    }

    fn drop_spare(&mut self, nav: NodeId) {
        let Some(navigator) = self.navigator(nav) else { return };
        let spare = core::mem::replace(&mut navigator.spare, NONE);
        if spare != NONE {
            self.hook(spare, Hook::Disappear);
            self.hook(spare, Hook::Exit);
            self.unmount(nav, spare);
        }
    }

    /// Runs `msg` on the component `id`, shown or covered. Covered, it has no
    /// nodes to rebuild or animate: its next view shows what the message did.
    pub(crate) fn post(&mut self, id: u32, msg: Erased) {
        if let Some(slot) = self.slots.iter().position(|s| s.host.as_ref().is_some_and(|host| host.id() == id)) {
            self.dispatch(slot as u16, msg, None);
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

    fn apply_covered(&mut self, id: u32, nav: NodeId, requests: Requests) {
        if let Some(theme) = requests.theme {
            self.set_theme(theme);
        }
        if requests.render.is_some() {
            idf::log::error!(c"ui", "a covered screen cannot change the render mode");
        }
        if !requests.popups.is_empty() {
            idf::log::error!(c"ui", "a covered screen cannot open a popup");
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

    fn hook(&mut self, node: NodeId, hook: Hook) {
        if node != NONE
            && let NodeContent::Child(slot) = self.nodes[node as usize].content
        {
            self.run_hook(slot, hook);
        }
    }

    fn top(&mut self, nav: NodeId) -> NodeId {
        self.navigator(nav).and_then(|n| n.stack.last()).map_or(NONE, |e| e.node)
    }

    fn fill_style() -> Style {
        Style { width: Dimension::Auto, height: Dimension::Auto, ..Style::default() }
    }

    /// Creates the nodes of `entry`'s screen, as a new last child of `nav`.
    pub(crate) fn mount(&mut self, nav: NodeId, host: Box<dyn AnyHost>, saved: Saved) -> NodeId {
        let parent_slot = self.nodes[nav as usize].owner;
        let inherited = self.slots.get(usize::from(parent_slot)).map_or(self.theme, |s| s.theme);
        self.mount_themed(nav, host, saved, inherited)
    }

    fn mount_themed(&mut self, nav: NodeId, host: Box<dyn AnyHost>, saved: Saved, inherited: &'static Theme) -> NodeId {
        let theme = host.theme();
        let slot = self.alloc_slot();
        let node = self.alloc_node(Node::new(NodeContent::Child(slot), slot, self.caps));
        self.nodes[node as usize].style = Self::fill_style();
        self.nodes[node as usize].parent = nav;
        let slot_index = usize::from(slot);
        self.slots[slot_index].host = Some(host);
        self.slots[slot_index].node = node;
        self.slots[slot_index].theme = theme.unwrap_or(inherited);
        self.restoring.push(saved);
        self.link_last(nav, node);
        self.build_component(slot);
        self.restoring.pop();
        self.layout_dirty = true;
        node
    }

    /// Shows `host` above everything, in the theme of the component `from`.
    pub(crate) fn open_popup(&mut self, from: u16, host: Box<dyn AnyHost>) {
        let theme = self.slots[usize::from(from)].theme;
        let node = self.mount_themed(self.popups, host, Saved::default(), theme);
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

    pub(crate) fn close_popups(&mut self) {
        loop {
            let popup = self.nodes[self.popups as usize].first_child;
            if popup == NONE {
                return;
            }
            self.unmount(self.popups, popup);
        }
    }

    fn link_last(&mut self, parent: NodeId, node: NodeId) {
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

    fn unlink(&mut self, parent: NodeId, node: NodeId) {
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

    fn collect(&mut self, id: NodeId, saved: &mut Saved) {
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            let next = self.nodes[child as usize].next;
            self.collect(child, saved);
            child = next;
        }
        match &mut self.nodes[id as usize].content {
            NodeContent::List(list) => saved.lists.push((list.id, list.offset)),
            NodeContent::Navigator(nav) => {
                let stack = core::mem::take(&mut nav.stack);
                let key = nav.key;
                saved.navigators.push((key, stack));
            }
            _ => {}
        }
    }

    /// Removes a screen's nodes, keeping what it needs to come back.
    fn unmount(&mut self, nav: NodeId, node: NodeId) -> (Option<Box<dyn AnyHost>>, Saved) {
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
        self.damage.push(self.nodes[node as usize].screen);
        self.unlink(nav, node);
        self.free_subtree(node);
        self.layout_dirty = true;
        (host, saved)
    }

    fn take_host(&mut self, node: NodeId) -> Option<Box<dyn AnyHost>> {
        match self.nodes[node as usize].content {
            NodeContent::Child(slot) => self.slots[usize::from(slot)].host.take(),
            _ => None,
        }
    }

    /// What a move shows: `asked`, then what the screen came with, then the
    /// navigator's and the theme's defaults. Without `defaulted`, only `asked`.
    fn pick(&self, nav: NodeId, asked: Option<Transition>, came_with: Option<Transition>, defaulted: bool) -> Transition {
        let NodeContent::Navigator(navigator) = &self.nodes[nav as usize].content else { return Transition::None };
        if navigator.key == TypeId::of::<Overlay>() {
            return Transition::None;
        }
        if !defaulted {
            return asked.unwrap_or(Transition::None);
        }
        asked.or(came_with).or(navigator.transition).unwrap_or(self.theme.motion.transition)
    }

    fn begin_transit(&mut self, nav: NodeId, leaving: NodeId, drop_leaving: bool, forward: bool, kind: Transition) {
        if leaving == NONE || kind == Transition::None {
            if leaving != NONE {
                self.retire(nav, leaving, drop_leaving);
            }
            return;
        }
        let duration = u32::from(self.theme.motion.duration_ms(kind)).max(1);
        if let Some(navigator) = self.navigator(nav) {
            navigator.transit = Some(Transit { kind, leaving, drop_leaving, elapsed: 0, duration, forward, zoom: None, skip_tick: false, stale: Vec::new() });
        }
        self.step_transit(nav, 0);
    }

    fn retire(&mut self, nav: NodeId, leaving: NodeId, drop_leaving: bool) {
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
    pub(crate) fn finish_transit(&mut self, nav: NodeId) -> bool {
        let Some(transit) = self.navigator(nav).and_then(|n| n.transit.take()) else { return false };
        let top = self.navigator(nav).map_or(NONE, |n| n.shown());
        if top != NONE {
            self.nodes[top as usize].translate = (0, 0);
        }
        self.retire(nav, transit.leaving, transit.drop_leaving);
        self.mark_dirty_rect(nav);
        true
    }

    fn mark_dirty_rect(&mut self, nav: NodeId) {
        let screen = self.nodes[nav as usize].screen;
        self.damage.push(screen);
        self.nodes[nav as usize].scrolled = true;
    }

    fn step_transit(&mut self, nav: NodeId, dt_ms: u32) {
        let top = self.navigator(nav).map_or(NONE, |n| n.shown());
        let side = self.navigator(nav).and_then(|n| n.split).unwrap_or(0);
        let (width, height) = (self.nodes[nav as usize].rect.w - side, self.nodes[nav as usize].rect.h);
        let Some(transit) = self.navigator(nav).and_then(|n| n.transit.as_mut()) else { return };
        let dt_ms = if core::mem::take(&mut transit.skip_tick) { 0 } else { dt_ms };
        transit.elapsed = (transit.elapsed + dt_ms).min(transit.duration);
        let (elapsed, duration, forward, leaving, kind) = (transit.elapsed, transit.duration, transit.forward, transit.leaving, transit.kind);
        let eased = ease_out(elapsed as f32 / duration as f32);
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
        if elapsed >= duration {
            self.finish_transit(nav);
        }
    }

    pub(crate) fn tick_transits(&mut self, dt_ms: u32) {
        // Finishing a transition can remove navigators inside the screen it hid.
        let mut i = 0;
        while let Some(&nav) = self.navigators.get(i) {
            self.step_transit(nav, dt_ms);
            i += 1;
        }
    }

    pub(crate) fn transiting(&self) -> bool {
        self.navigators.iter().any(|&nav| matches!(&self.nodes[nav as usize].content, NodeContent::Navigator(n) if n.transit.is_some()))
    }

    fn push_screen(&mut self, nav: NodeId, host: Box<dyn AnyHost>, asked: Option<Transition>) {
        // Pushing from the default detail keeps it to come back to.
        if let Some(navigator) = self.navigator(nav)
            && navigator.spare != NONE
        {
            let node = core::mem::replace(&mut navigator.spare, NONE);
            navigator.stack.push(Entry::shown(node, None));
            navigator.promoted = true;
        }
        let leaving = self.navigator(nav).map_or(NONE, |n| n.shown());
        self.hook(leaving, Hook::Disappear);
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry::shown(node, asked));
        }
        self.hook(node, Hook::Enter);
        self.hook(node, Hook::Appear);
        let kind = self.pick(nav, asked, None, true);
        self.begin_transit(nav, leaving, false, true, kind);
    }

    fn pop_screen(&mut self, nav: NodeId, asked: Option<Transition>) {
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
        self.hook(leaving, Hook::Disappear);
        self.hook(leaving, Hook::Exit);
        let top = if emptied { self.show_spare(nav) } else { self.show_top(nav) };
        if top == NONE {
            self.retire(nav, leaving, true);
            return;
        }
        // The leaving screen paints above the one coming back.
        self.unlink(nav, top);
        self.link_before(nav, top, leaving);
        let kind = self.pick(nav, asked, entry.transition, true);
        self.begin_transit(nav, leaving, true, false, kind);
    }

    fn link_before(&mut self, parent: NodeId, node: NodeId, before: NodeId) {
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
    fn show_screen(&mut self, nav: NodeId, host: Box<dyn AnyHost>, asked: Option<Transition>) {
        let link = self.link.clone();
        let Some(navigator) = self.navigator(nav) else { return };
        let (main, leaving, split) = (navigator.main(), navigator.shown(), navigator.split.is_some());
        let dropped = leaving != main;
        navigator.promoted = false;
        navigator.spare = NONE;
        if navigator.stack.len() > 1 {
            drop_to_root(&mut navigator.stack, &link);
        }
        self.hook(leaving, Hook::Disappear);
        if dropped {
            self.hook(leaving, Hook::Exit);
        }
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry::shown(node, asked));
        }
        self.hook(node, Hook::Enter);
        self.hook(node, Hook::Appear);
        // Next to the main screen this changes what is shown, not where we are.
        let kind = self.pick(nav, asked, None, !split);
        self.begin_transit(nav, leaving, dropped || split, true, kind);
    }

    fn show_top(&mut self, nav: NodeId) -> NodeId {
        let Some(navigator) = self.navigator(nav) else { return NONE };
        let Some(entry) = navigator.stack.last_mut() else { return NONE };
        let Some(host) = entry.host.take() else { return entry.node };
        let saved = core::mem::take(&mut entry.saved);
        let node = self.mount(nav, host, saved);
        if let Some(entry) = self.navigator(nav).and_then(|n| n.stack.last_mut()) {
            entry.node = node;
        }
        self.hook(node, Hook::Appear);
        node
    }

    fn replace_screen(&mut self, nav: NodeId, host: Box<dyn AnyHost>, asked: Option<Transition>) {
        let leaving = self.top(nav);
        self.hook(leaving, Hook::Disappear);
        self.hook(leaving, Hook::Exit);
        let Some(navigator) = self.navigator(nav) else { return };
        let main = navigator.split.is_some() && leaving == navigator.main();
        let came_with = navigator.stack.pop().and_then(|e| e.transition);
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry::shown(node, came_with));
        }
        self.hook(node, Hook::Enter);
        self.hook(node, Hook::Appear);
        // The main screen of a split navigator does not move.
        let kind = if main { Transition::None } else { self.pick(nav, asked, None, false) };
        self.begin_transit(nav, leaving, true, true, kind);
    }

    fn pop_to_root(&mut self, nav: NodeId, asked: Option<Transition>) {
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
    pub(crate) fn new_navigator(&mut self, key: TypeId, owner: u16) -> NodeId {
        let content = NodeContent::Navigator(NavigatorNode::new(key));
        let id = self.alloc_node(Node::new(content, owner, self.caps));
        self.nodes[id as usize].style.clip = true;
        self.navigators.push(id);
        id
    }

    pub(crate) fn seed_navigator(&mut self, nav: NodeId, host: Box<dyn AnyHost>) {
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry::shown(node, None));
        }
        self.hook(node, Hook::Enter);
        self.hook(node, Hook::Appear);
    }

    /// Gives a navigator its hidden stack back, if the screen around it kept one.
    pub(crate) fn restore_navigator(&mut self, nav: NodeId, key: TypeId) -> bool {
        let Some(saved) = self.restoring.last_mut() else { return false };
        let Some(at) = saved.navigators.iter().position(|(k, _)| *k == key) else { return false };
        let (_, stack) = saved.navigators.swap_remove(at);
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack = stack;
        }
        self.show_top(nav) != NONE
    }

    fn style_changed(&mut self, nav: NodeId) {
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
