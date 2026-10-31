/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::Rect;

use crate::canvas::{Canvas, CanvasId};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::{Any, TypeId};
use core::marker::PhantomData;

use crate::element::{Element, Ui};
use crate::erased::Erased;
use crate::nav::{Action, NavOp};
use crate::node::NodeId;
use crate::presenter::Strips;
use crate::style::ScreenStyle;
use crate::text_buffer::TextInput;
use crate::theme::Theme;
use crate::tree::Tree;

/// Which `Ui::list` of a component a row belongs to.
pub type ListId = u16;

/// A piece of UI with its own state. Messages are `Copy` and at most 16 bytes;
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

    /// Paints a `Ui::canvas`, in every frame that shows it.
    fn draw<'a>(&'a self, _id: CanvasId, _canvas: &mut Canvas<'a, '_>) {}
}

/// A component created by a parent's view.
pub trait Child: Component + Sized {
    fn create(props: &Self::Props) -> Self;

    /// The parent's view passed different props.
    fn set_props(&mut self, props: &Self::Props);
}

/// A component a navigator shows. Its state outlives its nodes while another
/// screen covers it, and scroll positions of lists come back with it; state of
/// child components does not.
pub trait Screen: Component + Sized {
    fn style(&self) -> ScreenStyle {
        ScreenStyle::default()
    }

    /// Replaces the theme for this screen and everything inside it.
    fn theme(&self) -> Option<&'static Theme> {
        None
    }

    /// Pushed onto a stack.
    fn on_enter(&mut self, _cx: &mut Cx<Self>) {}

    /// Removed from its stack for good.
    fn on_exit(&mut self, _cx: &mut Cx<Self>) {}

    /// Became the top of its stack.
    fn on_appear(&mut self, _cx: &mut Cx<Self>) {}

    /// Stopped being the top of its stack.
    fn on_disappear(&mut self, _cx: &mut Cx<Self>) {}
}

#[derive(Clone, Copy)]
pub(crate) enum Hook {
    Enter,
    Exit,
    Appear,
    Disappear,
}

pub(crate) struct RenderRequest {
    pub(crate) strips: Strips,
    pub(crate) lanes: usize,
    pub(crate) reply: Option<Erased>,
}

/// How the display is driven, apart from what the UI shows.
pub struct RenderMode {
    pub strips: Strips,
    pub lanes: usize,
}

#[derive(Default)]
pub(crate) struct Requests {
    pub(crate) rebuild: bool,
    pub(crate) sends: Vec<Erased>,
    pub(crate) events: Vec<Erased>,
    pub(crate) nav: Vec<NavOp>,
    pub(crate) render: Option<RenderRequest>,
    pub(crate) theme: Option<&'static Theme>,
    pub(crate) replaced: Option<Strips>,
    pub(crate) animations: Vec<(&'static str, Animation)>,
    pub(crate) focus: Option<&'static str>,
    pub(crate) input: Vec<TextInput>,
    pub(crate) redraw: Vec<(&'static str, Option<Rect>)>,
}

/// What an animation moves, without running a view or layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Property {
    TranslateX,
    TranslateY,
    Opacity,
}

/// `from` to `to`, easing out, over `duration_ms`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Animation {
    pub property: Property,
    pub from: i16,
    pub to: i16,
    pub duration_ms: u16,
}

/// What `Component::update` can ask for.
pub struct Cx<C: Component> {
    pub(crate) requests: Requests,
    _component: PhantomData<fn(C)>,
}

impl<C: Component> Cx<C> {
    pub(crate) fn new(replaced: Option<Strips>) -> Self {
        Self { requests: Requests { replaced, ..Requests::default() }, _component: PhantomData }
    }

    pub fn rebuild(&mut self) {
        self.requests.rebuild = true;
    }

    /// Delivers `msg` to this component after the current one.
    pub fn send(&mut self, msg: C::Msg) {
        self.requests.sends.push(Erased::new(msg));
    }

    /// Tells the parent, through the mapping given to `Ui::child`.
    pub fn emit(&mut self, event: C::Event) {
        self.requests.events.push(Erased::new(event));
    }

    /// The nearest navigator.
    pub fn nav(&mut self) -> Nav<'_> {
        Nav { requests: &mut self.requests, target: None }
    }

    /// The navigator of marker type `K`: the nearest above this component, then
    /// inside it, then anywhere.
    pub fn nav_of<K: 'static>(&mut self) -> Nav<'_> {
        Nav { requests: &mut self.requests, target: Some(TypeId::of::<K>()) }
    }

    /// Paints the canvas with this id again, without a view or layout.
    pub fn redraw(&mut self, id: &'static str) {
        self.requests.redraw.push((id, None));
    }

    /// The same for a part of it, in its coordinates.
    pub fn redraw_rect(&mut self, id: &'static str, rect: Rect) {
        self.requests.redraw.push((id, Some(rect)));
    }

    /// Moves the focus to the node with this id, after the view has been built.
    pub fn focus(&mut self, id: &'static str) {
        self.requests.focus = Some(id);
    }

    pub(crate) fn input(&mut self, input: TextInput) {
        self.requests.input.push(input);
    }

    /// Animates the node with this id, after the view has been built.
    pub fn animate(&mut self, id: &'static str, animation: Animation) {
        self.requests.animations.push((id, animation));
    }

    /// Replaces the theme of the whole UI.
    pub fn set_theme(&mut self, theme: &'static Theme) {
        self.requests.theme = Some(theme);
    }

    /// Changes the render mode before the next frame. `done` then arrives, with
    /// the memory given up in [`Self::take_strips`]; a mode that cannot work
    /// leaves things as they are and hands its own memory back instead.
    pub fn set_render_mode(&mut self, mode: RenderMode, done: C::Msg) {
        let reply = Some(Erased::new(done));
        self.requests.render = Some(RenderRequest { strips: mode.strips, lanes: mode.lanes, reply });
    }

    pub fn take_strips(&mut self) -> Option<Strips> {
        self.requests.replaced.take()
    }
}

/// Moves between screens, after the current `update`.
pub struct Nav<'a> {
    requests: &'a mut Requests,
    target: Option<TypeId>,
}

impl Nav<'_> {
    fn add(&mut self, action: Action) {
        self.requests.nav.push(NavOp { target: self.target, action });
    }

    pub fn push<S: Screen>(&mut self, screen: S) {
        self.add(Action::Push(screen_host(screen)));
    }

    /// Without a marker type, the innermost navigator with something to pop.
    pub fn pop(&mut self) {
        self.add(Action::Pop);
    }

    pub fn replace<S: Screen>(&mut self, screen: S) {
        self.add(Action::Replace(screen_host(screen)));
    }

    /// Back to the bottom of the stack.
    pub fn root(&mut self) {
        self.add(Action::Root);
    }
}

pub(crate) fn screen_host<S: Screen>(screen: S) -> Box<dyn AnyHost> {
    Box::new(ScreenHost { inner: Host { component: screen, props: None } })
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

/// A component without its type.
pub(crate) trait AnyHost {
    fn dispatch(&mut self, msg: Erased, replaced: Option<Strips>) -> Requests;
    fn build(&self, tree: &mut Tree, id: u16, node: NodeId);
    fn build_row(&self, tree: &mut Tree, id: u16, list: ListId, index: usize, row: NodeId);
    fn draw<'a>(&'a self, id: CanvasId, canvas: &mut Canvas<'a, '_>);
    fn as_any(&mut self) -> &mut dyn Any;

    fn hook(&mut self, _hook: Hook) -> Requests {
        Requests::default()
    }

    fn screen_style(&self) -> Option<ScreenStyle> {
        None
    }

    fn theme(&self) -> Option<&'static Theme> {
        None
    }
}

pub(crate) struct Host<C: Component> {
    pub(crate) component: C,
    pub(crate) props: Option<C::Props>,
}

impl<C: Component> AnyHost for Host<C> {
    fn dispatch(&mut self, msg: Erased, replaced: Option<Strips>) -> Requests {
        let mut cx = Cx::<C>::new(replaced);
        self.component.update(unsafe { msg.get::<C::Msg>() }, &mut cx);
        cx.requests
    }

    fn build(&self, tree: &mut Tree, id: u16, node: NodeId) {
        let ui = Ui::new(tree.bump(), tree.context(id));
        let element = self.component.view(&ui);
        tree.reconcile(id, node, &element);
    }

    fn build_row(&self, tree: &mut Tree, id: u16, list: ListId, index: usize, row: NodeId) {
        let ui = Ui::new(tree.bump(), tree.context(id));
        let element = self.component.list_row(list, index, &ui);
        tree.reconcile(id, row, &element);
    }

    fn draw<'a>(&'a self, id: CanvasId, canvas: &mut Canvas<'a, '_>) {
        self.component.draw(id, canvas);
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

pub(crate) struct ScreenHost<S: Screen> {
    pub(crate) inner: Host<S>,
}

impl<S: Screen> AnyHost for ScreenHost<S> {
    fn dispatch(&mut self, msg: Erased, replaced: Option<Strips>) -> Requests {
        self.inner.dispatch(msg, replaced)
    }

    fn draw<'a>(&'a self, id: CanvasId, canvas: &mut Canvas<'a, '_>) {
        self.inner.draw(id, canvas);
    }

    fn build(&self, tree: &mut Tree, id: u16, node: NodeId) {
        self.inner.build(tree, id, node);
    }

    fn build_row(&self, tree: &mut Tree, id: u16, list: ListId, index: usize, row: NodeId) {
        self.inner.build_row(tree, id, list, index, row);
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn hook(&mut self, hook: Hook) -> Requests {
        let mut cx = Cx::<S>::new(None);
        let screen = &mut self.inner.component;
        match hook {
            Hook::Enter => screen.on_enter(&mut cx),
            Hook::Exit => screen.on_exit(&mut cx),
            Hook::Appear => screen.on_appear(&mut cx),
            Hook::Disappear => screen.on_disappear(&mut cx),
        }
        cx.requests
    }

    fn screen_style(&self) -> Option<ScreenStyle> {
        Some(self.inner.component.style())
    }

    fn theme(&self) -> Option<&'static Theme> {
        self.inner.component.theme()
    }
}
