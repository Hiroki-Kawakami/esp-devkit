/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::TypeId;

use crate::component::{AnyHost, Hook};
use crate::node::{NONE, Node, NodeContent, NodeId};
use crate::style::{Dimension, Style};
use crate::theme::Transition;
use crate::tree::Tree;

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
}

pub(crate) struct Slide {
    pub(crate) leaving: NodeId,
    /// The leaving screen is gone for good, as after `pop`.
    pub(crate) drop_leaving: bool,
    pub(crate) elapsed: u32,
    pub(crate) duration: u32,
    pub(crate) forward: bool,
}

pub(crate) struct NavigatorNode {
    pub(crate) key: TypeId,
    pub(crate) stack: Vec<Entry>,
    pub(crate) slide: Option<Slide>,
}

pub(crate) enum Action {
    Push(Box<dyn AnyHost>),
    Pop,
    Replace(Box<dyn AnyHost>),
    Root,
}

pub(crate) struct NavOp {
    pub(crate) target: Option<TypeId>,
    pub(crate) action: Action,
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
        let node = self.slots[usize::from(from)].node;
        let popping = matches!(op.action, Action::Pop);
        let Some(nav) = self.resolve(node, op.target, popping) else {
            idf::log::error!(c"ui", "no navigator for this operation");
            return;
        };
        self.finish_slide(nav);
        match op.action {
            Action::Push(host) => self.push_screen(nav, host),
            Action::Pop => self.pop_screen(nav),
            Action::Replace(host) => {
                self.leave_top(nav);
                self.show_top_replaced(nav, host);
            }
            Action::Root => self.pop_to_root(nav),
        }
        self.style_changed(nav);
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
        let theme = host.theme();
        let parent_slot = self.nodes[nav as usize].owner;
        let slot = self.alloc_slot();
        let node = self.alloc_node(Node::new(NodeContent::Child(slot), slot));
        self.nodes[node as usize].style = Self::fill_style();
        self.nodes[node as usize].parent = nav;
        let inherited = self.slots.get(usize::from(parent_slot)).map_or(self.theme, |s| s.theme);
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
            if let Some(entry) = stack.last_mut() {
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

    fn begin_slide(&mut self, nav: NodeId, leaving: NodeId, drop_leaving: bool, forward: bool) {
        let theme = self.theme;
        if leaving == NONE || theme.motion.transition == Transition::None {
            if leaving != NONE {
                self.retire(nav, leaving, drop_leaving);
            }
            return;
        }
        let duration = u32::from(theme.motion.duration_ms);
        if let Some(navigator) = self.navigator(nav) {
            navigator.slide = Some(Slide { leaving, drop_leaving, elapsed: 0, duration, forward });
        }
        self.step_slide(nav, 0);
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

    pub(crate) fn finish_slide(&mut self, nav: NodeId) {
        let Some(slide) = self.navigator(nav).and_then(|n| n.slide.take()) else { return };
        let top = self.top(nav);
        if top != NONE {
            self.nodes[top as usize].translate = (0, 0);
        }
        self.retire(nav, slide.leaving, slide.drop_leaving);
        self.mark_dirty_rect(nav);
    }

    fn mark_dirty_rect(&mut self, nav: NodeId) {
        let screen = self.nodes[nav as usize].screen;
        self.damage.push(screen);
        self.nodes[nav as usize].scrolled = true;
    }

    fn step_slide(&mut self, nav: NodeId, dt_ms: u32) {
        let top = self.top(nav);
        let width = self.nodes[nav as usize].rect.w;
        let Some(slide) = self.navigator(nav).and_then(|n| n.slide.as_mut()) else { return };
        slide.elapsed = (slide.elapsed + dt_ms).min(slide.duration);
        let (elapsed, duration, forward, leaving) = (slide.elapsed, slide.duration, slide.forward, slide.leaving);
        let t = elapsed as f32 / duration as f32;
        let eased = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
        let shift = (f32::from(width) * eased) as i16;
        let (entering, departing) = if forward { (width - shift, -shift / 3) } else { (-(width - shift) / 3, shift) };
        self.nodes[top as usize].translate = (entering, 0);
        self.nodes[leaving as usize].translate = (departing, 0);
        self.mark_dirty_rect(nav);
        if elapsed >= duration {
            self.finish_slide(nav);
        }
    }

    pub(crate) fn tick_slides(&mut self, dt_ms: u32) {
        // Finishing a slide can remove navigators inside the screen it hid.
        let mut i = 0;
        while let Some(&nav) = self.navigators.get(i) {
            self.step_slide(nav, dt_ms);
            i += 1;
        }
    }

    pub(crate) fn sliding(&self) -> bool {
        self.navigators.iter().any(|&nav| matches!(&self.nodes[nav as usize].content, NodeContent::Navigator(n) if n.slide.is_some()))
    }

    fn push_screen(&mut self, nav: NodeId, host: Box<dyn AnyHost>) {
        let leaving = self.top(nav);
        self.hook(leaving, Hook::Disappear);
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry { host: None, node, saved: Saved::default() });
        }
        self.hook(node, Hook::Enter);
        self.hook(node, Hook::Appear);
        self.begin_slide(nav, leaving, false, true);
    }

    fn pop_screen(&mut self, nav: NodeId) {
        let Some(navigator) = self.navigator(nav) else { return };
        let empties = navigator.key == TypeId::of::<Overlay>();
        if navigator.stack.len() < 2 && !empties {
            return;
        }
        let Some(entry) = navigator.stack.pop() else { return };
        let leaving = entry.node;
        self.hook(leaving, Hook::Disappear);
        self.hook(leaving, Hook::Exit);
        if self.show_top(nav) == NONE {
            self.retire(nav, leaving, true);
            return;
        }
        // The leaving screen paints above the one coming back.
        let top = self.top(nav);
        self.unlink(nav, top);
        self.nodes[top as usize].next = leaving;
        self.nodes[nav as usize].first_child = top;
        self.begin_slide(nav, leaving, true, false);
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

    fn leave_top(&mut self, nav: NodeId) {
        let leaving = self.top(nav);
        self.hook(leaving, Hook::Disappear);
        self.hook(leaving, Hook::Exit);
        if leaving != NONE {
            self.unmount(nav, leaving);
        }
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.pop();
        }
    }

    fn show_top_replaced(&mut self, nav: NodeId, host: Box<dyn AnyHost>) {
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry { host: None, node, saved: Saved::default() });
        }
        self.hook(node, Hook::Enter);
        self.hook(node, Hook::Appear);
    }

    fn pop_to_root(&mut self, nav: NodeId) {
        let Some(navigator) = self.navigator(nav) else { return };
        if navigator.stack.len() < 2 {
            return;
        }
        let shown = navigator.stack.len() - 1;
        navigator.stack.drain(1..shown);
        self.pop_screen(nav);
    }

    /// Fills `area` with a navigator node owned by `owner`.
    pub(crate) fn new_navigator(&mut self, key: TypeId, owner: u16) -> NodeId {
        let content = NodeContent::Navigator(NavigatorNode { key, stack: Vec::new(), slide: None });
        let id = self.alloc_node(Node::new(content, owner));
        self.nodes[id as usize].style.clip = true;
        self.navigators.push(id);
        id
    }

    pub(crate) fn seed_navigator(&mut self, nav: NodeId, host: Box<dyn AnyHost>) {
        let node = self.mount(nav, host, Saved::default());
        if let Some(navigator) = self.navigator(nav) {
            navigator.stack.push(Entry { host: None, node, saved: Saved::default() });
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
