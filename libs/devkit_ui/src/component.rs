/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::any::Any;
use core::marker::PhantomData;

use crate::element::{Element, Ui};
use crate::erased::Erased;
use crate::node::NodeId;
use crate::tree::Tree;

/// Which `Ui::list` of a component a row belongs to.
pub type ListId = u16;

/// A piece of UI with its own state. Messages are `Copy` and at most 8 bytes;
/// larger payloads stay in the state and messages carry an index.
pub trait Component: 'static {
    type Msg: Copy + 'static;
    type Props: Copy + PartialEq + 'static = ();
    type Event: Copy + 'static = ();

    /// Call `cx.rebuild()` when the view changed.
    fn update(&mut self, msg: Self::Msg, cx: &mut Cx<Self>)
    where
        Self: Sized;

    fn view<'a>(&'a self, ui: &'a Ui<Self::Msg>) -> Element<'a, Self::Msg>;

    fn list_row<'a>(&'a self, _list: ListId, _index: usize, ui: &'a Ui<Self::Msg>) -> Element<'a, Self::Msg> {
        ui.empty()
    }
}

/// A component created by a parent's view.
pub trait Child: Component + Sized {
    fn create(props: &Self::Props) -> Self;

    /// The parent's view passed different props.
    fn set_props(&mut self, props: &Self::Props);
}

/// What `Component::update` can ask for.
pub struct Cx<C: Component> {
    pub(crate) rebuild: bool,
    pub(crate) sends: Vec<Erased>,
    pub(crate) events: Vec<Erased>,
    _component: PhantomData<fn(C)>,
}

impl<C: Component> Cx<C> {
    pub(crate) fn new() -> Self {
        Self { rebuild: false, sends: Vec::new(), events: Vec::new(), _component: PhantomData }
    }

    pub fn rebuild(&mut self) {
        self.rebuild = true;
    }

    /// Delivers `msg` to this component after the current one.
    pub fn send(&mut self, msg: C::Msg) {
        self.sends.push(Erased::new(msg));
    }

    /// Tells the parent, through the mapping given to `Ui::child`.
    pub fn emit(&mut self, event: C::Event) {
        self.events.push(Erased::new(event));
    }
}

pub(crate) struct EventMap {
    pub(crate) parent: u16,
    pub(crate) function: *const (),
    pub(crate) call: unsafe fn(*const (), Erased) -> Erased,
}

pub(crate) unsafe fn call_event_map<E: Copy, M: Copy>(function: *const (), event: Erased) -> Erased {
    let function: fn(E) -> M = unsafe { core::mem::transmute(function) };
    Erased::new(function(unsafe { event.get::<E>() }))
}

pub(crate) struct Update {
    pub(crate) rebuild: bool,
    pub(crate) sends: Vec<Erased>,
    pub(crate) events: Vec<Erased>,
}

/// A component without its type.
pub(crate) trait AnyHost {
    fn dispatch(&mut self, msg: Erased) -> Update;
    fn build(&self, tree: &mut Tree, id: u16, node: NodeId);
    fn build_row(&self, tree: &mut Tree, id: u16, list: ListId, index: usize, row: NodeId);
    fn as_any(&mut self) -> &mut dyn Any;
}

pub(crate) struct Host<C: Component> {
    pub(crate) component: C,
    pub(crate) props: Option<C::Props>,
}

impl<C: Component> AnyHost for Host<C> {
    fn dispatch(&mut self, msg: Erased) -> Update {
        let mut cx = Cx::<C>::new();
        self.component.update(unsafe { msg.get::<C::Msg>() }, &mut cx);
        Update { rebuild: cx.rebuild, sends: cx.sends, events: cx.events }
    }

    fn build(&self, tree: &mut Tree, id: u16, node: NodeId) {
        let ui = Ui::new(tree.bump());
        let element = self.component.view(&ui);
        tree.reconcile(id, node, &element);
    }

    fn build_row(&self, tree: &mut Tree, id: u16, list: ListId, index: usize, row: NodeId) {
        let ui = Ui::new(tree.bump());
        let element = self.component.list_row(list, index, &ui);
        tree.reconcile(id, row, &element);
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}
