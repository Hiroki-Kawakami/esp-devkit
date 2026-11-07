/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::sync::atomic::{AtomicU32, Ordering};

use bsp_rs::display;
use devkit_gfx::Rect;

use crate::app::{Handle, Link};
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::any::{Any, TypeId};
use core::future::Future;
use core::marker::PhantomData;

use crate::element::{Element, Ui};
use crate::erased::Erased;
use crate::id::Id;
use crate::nav::{Action, NavOp};
use crate::node::NodeIndex;
use crate::presenter::Strips;
use crate::style::ScreenStyle;
use crate::task::{Local, Task, This};
use crate::text_buffer::TextInput;
use crate::theme::{Theme, Transition};
use crate::tree::Tree;

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
}

/// A component created by a parent's view.
pub trait Child: Component + Sized {
    fn create(props: &Self::Props) -> Self;

    /// The parent's view passed different props.
    fn set_props(&mut self, props: &Self::Props);
}

/// A component a navigator shows. Its state outlives its nodes while another
/// screen covers it, and scroll positions of lists come back with it; state of
/// child components does not. It is dropped once it leaves its stack.
///
/// The appear and disappear hooks come in that order and in pairs. Moves
/// between screens asked from any hook wait until the one under way is done.
pub trait Screen: Component + Sized {
    fn style(&self) -> ScreenStyle {
        ScreenStyle::default()
    }

    /// Replaces the theme for this screen and everything inside it.
    fn theme(&self) -> Option<&'static Theme> {
        None
    }

    /// Sent when the scrim around it on an `Overlay` is tapped; `None` only
    /// keeps the tap from what is below.
    fn scrim_tap(&self) -> Option<Self::Msg> {
        None
    }

    /// Once, before anything else, with the first `cx` it gets.
    fn on_load(&mut self, _cx: &mut Cx<Self>) {}

    /// Before the view it is about to show is built.
    fn will_appear(&mut self, _cx: &mut Cx<Self>) {}

    /// Once it is in: after its transition, or right away without one.
    fn did_appear(&mut self, _cx: &mut Cx<Self>) {}

    /// As it starts to go, covered or removed.
    fn will_disappear(&mut self, _cx: &mut Cx<Self>) {}

    /// Once its nodes are gone.
    fn did_disappear(&mut self, _cx: &mut Cx<Self>) {}
}

#[derive(Clone, Copy)]
pub(crate) enum Hook {
    WillAppear,
    DidAppear,
    WillDisappear,
    DidDisappear,
}

pub(crate) struct RenderRequest {
    pub(crate) strips: Strips,
    pub(crate) lanes: usize,
    pub(crate) display: display::Config,
    pub(crate) reply: Option<Erased>,
}

/// How the display is driven, apart from what the UI shows.
pub struct RenderMode {
    pub strips: Strips,
    pub lanes: usize,
    /// Pixel format and framebuffer count; 0 and `None` keep what the display
    /// has.
    pub display: display::Config,
}

#[derive(Default)]
pub(crate) struct Requests {
    pub(crate) rebuild: bool,
    pub(crate) sends: Vec<Erased>,
    pub(crate) posts: Vec<(u32, Erased)>,
    pub(crate) events: Vec<Erased>,
    pub(crate) nav: Vec<NavOp>,
    pub(crate) popups: Vec<Box<dyn AnyHost>>,
    pub(crate) close_popup: bool,
    pub(crate) render: Option<RenderRequest>,
    pub(crate) theme: Option<&'static Theme>,
    pub(crate) replaced: Option<Strips>,
    pub(crate) tap_rect: Option<Rect>,
    pub(crate) animations: Vec<(Id, Animation)>,
    pub(crate) focus: Option<Id>,
    pub(crate) input: Vec<TextInput>,
    pub(crate) redraw: Vec<(Id, Option<Rect>)>,
    pub(crate) scrolls: Vec<(Id, ScrollTarget, bool)>,
    pub(crate) guards: Vec<Box<dyn Any>>,
    pub(crate) tasks: Vec<Task>,
}

/// Where `Cx::scroll_to` moves a `scroll` or `list`. `Index` is a row of a
/// `list`, shown whole with as little movement as it takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScrollTarget {
    Start,
    End,
    Offset(i32),
    Index(usize),
}

/// The end a `scroll` or `list` starts at, and stays at while its content or
/// size changes if it was there.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ScrollEdge {
    #[default]
    Start,
    End,
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
    id: u32,
    link: Link,
    local: Rc<Local>,
    _component: PhantomData<fn(C)>,
}

impl<C: Component> Cx<C> {
    pub(crate) fn new(replaced: Option<Strips>, tap_rect: Option<Rect>, id: u32, link: &Link, local: &Rc<Local>) -> Self {
        let requests = Requests { replaced, tap_rect, ..Requests::default() };
        Self { requests, id, link: link.clone(), local: local.clone(), _component: PhantomData }
    }

    /// Reaches this component from any task, for as long as it lives, whether
    /// a screen covers it or not.
    pub fn handle(&self) -> Handle<C::Msg> {
        Handle::new(self.link.clone(), self.id)
    }

    pub fn rebuild(&mut self) {
        self.requests.rebuild = true;
    }

    /// Delivers `msg` to this component after the current one.
    pub fn send(&mut self, msg: C::Msg) {
        self.requests.sends.push(Erased::new(msg));
    }

    /// Delivers `msg` to the component of `target` after the current one.
    pub fn send_to<M: Copy + 'static>(&mut self, target: &Handle<M>, msg: M) {
        self.requests.posts.push((target.id(), Erased::new(msg)));
    }

    /// Tells the parent, through the mapping given to `Ui::child`.
    pub fn emit(&mut self, event: C::Event) {
        self.requests.events.push(Erased::new(event));
    }

    /// The nearest navigator.
    pub fn nav(&mut self) -> Nav<'_> {
        Nav { requests: &mut self.requests, target: None, transition: None }
    }

    /// The navigator of marker type `K`: the nearest above this component, then
    /// inside it, then anywhere.
    pub fn nav_of<K: 'static>(&mut self) -> Nav<'_> {
        Nav { requests: &mut self.requests, target: Some(TypeId::of::<K>()), transition: None }
    }

    /// Where the node tapped for this message is on the screen, through `send`
    /// and `emit` too. `None` for what no tap led to.
    pub fn tap_rect(&self) -> Option<Rect> {
        self.requests.tap_rect
    }

    /// Shows `component` above everything, keeping what is under it. It fills
    /// the screen, and places itself; a turn of the screen closes it.
    pub fn popup<P: Component>(&mut self, component: P) {
        self.requests.popups.push(Box::new(Host::<P>::new(component, None)));
    }

    /// Closes the popup this component is part of.
    pub fn close_popup(&mut self) {
        self.requests.close_popup = true;
    }

    /// Paints the canvas with this id again, without a view or layout.
    pub fn redraw(&mut self, id: impl Into<Id>) {
        self.requests.redraw.push((id.into(), None));
    }

    /// The same for a part of it, in its coordinates.
    pub fn redraw_rect(&mut self, id: impl Into<Id>, rect: Rect) {
        self.requests.redraw.push((id.into(), Some(rect)));
    }

    /// Scrolls the `scroll` or `list` with this id after the view and layout,
    /// easing there when `animated`. A finger on it wins.
    pub fn scroll_to(&mut self, id: impl Into<Id>, target: ScrollTarget, animated: bool) {
        self.requests.scrolls.push((id.into(), target, animated));
    }

    /// Moves the focus to the node with this id, after the view has been built.
    pub fn focus(&mut self, id: impl Into<Id>) {
        self.requests.focus = Some(id.into());
    }

    pub(crate) fn input(&mut self, input: TextInput) {
        self.requests.input.push(input);
    }

    /// Animates the node with this id, after the view has been built.
    pub fn animate(&mut self, id: impl Into<Id>, animation: Animation) {
        self.requests.animations.push((id.into(), animation));
    }

    /// Replaces the theme of the whole UI.
    pub fn set_theme(&mut self, theme: &'static Theme) {
        self.requests.theme = Some(theme);
    }

    /// Changes the render mode before the next frame; a display change also
    /// waits for screen transitions and a surface frame being written to end.
    /// `done` then arrives, with the memory given up in [`Self::take_strips`];
    /// a mode that cannot work leaves things as they are and hands its own
    /// memory back instead.
    pub fn set_render_mode(&mut self, mode: RenderMode, done: C::Msg) {
        let reply = Some(Erased::new(done));
        self.requests.render = Some(RenderRequest { strips: mode.strips, lanes: mode.lanes, display: mode.display, reply });
    }

    pub fn take_strips(&mut self) -> Option<Strips> {
        self.requests.replaced.take()
    }

    /// Runs `f`'s future on the UI task, between frames, until it ends, the
    /// `Task` aborts it or this component goes.
    pub fn spawn<F, Fut>(&mut self, f: F) -> Task
    where
        F: FnOnce(This<C>) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        let task = Task::new(self.local.executor.spawn_local(f(This::new(self.id, &self.local))));
        self.requests.tasks.push(task.clone());
        task
    }
}

impl<C: Screen> Cx<C> {
    /// Keeps `guard` until the screen starts to disappear.
    pub fn while_shown<G: 'static>(&mut self, guard: G) {
        self.requests.guards.push(Box::new(guard));
    }
}

/// Moves between screens, after the current `update`.
pub struct Nav<'a> {
    requests: &'a mut Requests,
    target: Option<TypeId>,
    transition: Option<Transition>,
}

impl Nav<'_> {
    fn add(&mut self, action: Action) {
        self.requests.nav.push(NavOp { target: self.target, action, transition: self.transition });
    }

    /// For the moves after this, over the navigator's default. A `pop` without
    /// one plays what the screen came with backwards. `replace`, and `show`
    /// beside a split main screen, change at once unless asked.
    pub fn transition(mut self, transition: Transition) -> Self {
        self.transition = Some(transition);
        self
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

    /// Replaces everything above the bottom of the stack, which is how a split
    /// navigator's main screen opens something on the right.
    pub fn show<S: Screen>(&mut self, screen: S) {
        self.add(Action::Show(screen_host(screen)));
    }

    /// Back to the bottom of the stack.
    pub fn root(&mut self) {
        self.add(Action::Root);
    }
}

pub(crate) fn screen_host<S: Screen>(screen: S) -> Box<dyn AnyHost> {
    Box::new(ScreenHost { inner: Host::new(screen, None), loaded: false })
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
    fn id(&self) -> u32;
    fn dispatch(&mut self, msg: Erased, replaced: Option<Strips>, tap_rect: Option<Rect>, link: &Link, local: &Rc<Local>) -> Requests;
    fn build(&self, tree: &mut Tree, id: u16, node: NodeIndex);
    fn component(&self) -> &dyn Any;
    fn component_mut(&mut self) -> &mut dyn Any;
    fn as_any(&mut self) -> &mut dyn Any;
    /// Keeps what `Cx::spawn` started for as long as the component lives.
    fn adopt(&mut self, tasks: &mut Vec<Task>);

    fn hook(&mut self, _hook: Hook, _link: &Link, _local: &Rc<Local>) -> Requests {
        Requests::default()
    }

    fn screen_style(&self) -> Option<ScreenStyle> {
        None
    }

    fn theme(&self) -> Option<&'static Theme> {
        None
    }

    fn scrim_tap(&self) -> Option<Erased> {
        None
    }
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

pub(crate) struct Host<C: Component> {
    id: u32,
    pub(crate) component: C,
    pub(crate) props: Option<C::Props>,
    tasks: Vec<Task>,
}

impl<C: Component> Host<C> {
    pub(crate) fn new(component: C, props: Option<C::Props>) -> Self {
        Self { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), component, props, tasks: Vec::new() }
    }
}

impl<C: Component> Drop for Host<C> {
    fn drop(&mut self) {
        self.tasks.iter().for_each(Task::abort);
    }
}

impl<C: Component> AnyHost for Host<C> {
    fn id(&self) -> u32 {
        self.id
    }

    fn dispatch(&mut self, msg: Erased, replaced: Option<Strips>, tap_rect: Option<Rect>, link: &Link, local: &Rc<Local>) -> Requests {
        let mut cx = Cx::<C>::new(replaced, tap_rect, self.id, link, local);
        self.component.update(unsafe { msg.get::<C::Msg>() }, &mut cx);
        self.adopt(&mut cx.requests.tasks);
        cx.requests
    }

    fn build(&self, tree: &mut Tree, id: u16, node: NodeIndex) {
        let ui = Ui::new(tree.bump(), tree.context(id));
        let element = self.component.view(&ui);
        tree.reconcile(id, node, &element);
    }

    fn component(&self) -> &dyn Any {
        &self.component
    }

    fn component_mut(&mut self) -> &mut dyn Any {
        &mut self.component
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn adopt(&mut self, tasks: &mut Vec<Task>) {
        if !tasks.is_empty() {
            self.tasks.retain(|task| !task.is_finished());
            self.tasks.append(tasks);
        }
    }
}

pub(crate) struct ScreenHost<S: Screen> {
    pub(crate) inner: Host<S>,
    loaded: bool,
}

impl<S: Screen> AnyHost for ScreenHost<S> {
    fn id(&self) -> u32 {
        self.inner.id()
    }

    fn dispatch(&mut self, msg: Erased, replaced: Option<Strips>, tap_rect: Option<Rect>, link: &Link, local: &Rc<Local>) -> Requests {
        self.inner.dispatch(msg, replaced, tap_rect, link, local)
    }

    fn build(&self, tree: &mut Tree, id: u16, node: NodeIndex) {
        self.inner.build(tree, id, node);
    }

    fn component(&self) -> &dyn Any {
        self.inner.component()
    }

    fn component_mut(&mut self) -> &mut dyn Any {
        self.inner.component_mut()
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }

    fn adopt(&mut self, tasks: &mut Vec<Task>) {
        self.inner.adopt(tasks);
    }

    fn hook(&mut self, hook: Hook, link: &Link, local: &Rc<Local>) -> Requests {
        let mut cx = Cx::<S>::new(None, None, self.inner.id(), link, local);
        let screen = &mut self.inner.component;
        match hook {
            Hook::WillAppear => {
                if !core::mem::replace(&mut self.loaded, true) {
                    screen.on_load(&mut cx);
                }
                screen.will_appear(&mut cx);
            }
            Hook::DidAppear => screen.did_appear(&mut cx),
            Hook::WillDisappear => screen.will_disappear(&mut cx),
            Hook::DidDisappear => screen.did_disappear(&mut cx),
        }
        self.inner.adopt(&mut cx.requests.tasks);
        cx.requests
    }

    fn screen_style(&self) -> Option<ScreenStyle> {
        Some(self.inner.component.style())
    }

    fn theme(&self) -> Option<&'static Theme> {
        self.inner.component.theme()
    }

    fn scrim_tap(&self) -> Option<Erased> {
        self.inner.component.scrim_tap().map(Erased::new)
    }
}
