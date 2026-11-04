/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::{Any, TypeId};
use core::fmt::{self, Write};
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::{self, NonNull};

use devkit_gfx::{Align as TextAlign, Color, Corners, Elide, FontChain, Image, ImageFormat, Overflow, Sides, TextOptions};

use crate::bump::Bump;
use crate::popover::Anchored;
use crate::component::{AnyHost, Child, Component, EventMap, Host, Screen, ScrollEdge, call_event_map, screen_host};
use crate::erased::Erased;
use crate::id::Id;
use crate::inbox::{Inbox, Reply, Token};
use crate::style::{Align, Animated, Direction, Inset, Insets, Justify, Part, Prop, Role, ScreenStyle, Shadow, State, StateStyle, Style};
use crate::canvas::Canvas;
use crate::external::ExternalSurface;
use crate::node::{NodeIndex, Pull};
use crate::nav::{DetailMap, SplitRole, SplitSpec, call_detail_map};
use crate::presence::Presence;
use crate::progress::Gauge;
use crate::gesture::{Gesture, GestureMap, Gestures, call_gesture_map};
use crate::keyboard::{InputKind, Keyboard};
use crate::text_buffer::TextInput;
use crate::theme::{ColorMode, ColorRole, Icon, Palette, Theme, Transition};
use crate::tree::Tree;

/// Creates or updates a child component during reconciliation.
pub(crate) trait ChildSpec {
    fn host_type(&self) -> TypeId;
    fn create(&self) -> Box<dyn AnyHost>;
    /// `true` when the view must be rebuilt.
    fn update(&self, host: &mut dyn AnyHost) -> bool;
    fn event_map(&self, parent: u16) -> Option<EventMap>;
}

pub struct ChildElement<C: Child, M: Copy> {
    props: C::Props,
    on_event: Option<fn(C::Event) -> M>,
}

impl<C: Child, M: Copy + 'static> ChildSpec for ChildElement<C, M> {
    fn host_type(&self) -> TypeId {
        TypeId::of::<Host<C>>()
    }

    fn create(&self) -> Box<dyn AnyHost> {
        Box::new(Host::new(C::create(&self.props), Some(self.props)))
    }

    fn update(&self, host: &mut dyn AnyHost) -> bool {
        let host = host.as_any().downcast_mut::<Host<C>>().expect("same type");
        if host.props == Some(self.props) {
            return false;
        }
        host.props = Some(self.props);
        host.component.set_props(&self.props);
        true
    }

    fn event_map(&self, parent: u16) -> Option<EventMap> {
        let function = self.on_event?;
        Some(EventMap { parent, function: function as *const (), call: call_event_map::<C::Event, M> })
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Content<'a> {
    Empty,
    Box,
    Text { font: &'static FontChain, text: &'a str, options: TextOptions, edit: Option<Edit> },
    Image { image: Image<'static>, tint: Color },
    Scroll,
    Collection { count: usize, layout: CollectionLayout, revision: Option<u32>, overscan: u8, item: ItemMap },
    Canvas(DrawMap),
    External(&'a ExternalSurface),
    Progress { gauge: Gauge, value: Option<f32>, period_ms: u16 },
    Child(&'a dyn ChildSpec),
    Navigator { key: TypeId, initial: Option<&'a dyn ChildSpec>, split: SplitSpec, transition: Option<Transition> },
}

#[derive(Clone, Copy)]
pub(crate) struct SizeMap {
    pub(crate) function: *const (),
    pub(crate) call: unsafe fn(*const (), i16, i16) -> Erased,
}

unsafe fn call_size_map<M: Copy>(function: *const (), width: i16, height: i16) -> Erased {
    let function: fn(i16, i16) -> M = unsafe { core::mem::transmute(function) };
    Erased::new(function(width, height))
}

/// What an editable text shows besides the text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Edit {
    pub(crate) cursor: u32,
    pub(crate) underline: Option<(u32, u32)>,
    /// Shown as a mark per character.
    pub(crate) secure: bool,
}

/// Where a focused input's text comes from.
#[derive(Clone, Copy)]
pub(crate) struct InputMap {
    pub(crate) function: *const (),
    pub(crate) call: unsafe fn(*const (), TextInput) -> Erased,
}

unsafe fn call_input_map<M: Copy>(function: *const (), input: TextInput) -> Erased {
    let function: fn(TextInput) -> M = unsafe { core::mem::transmute(function) };
    Erased::new(function(input))
}

/// A drag on a node, in its area inside the padding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Drag {
    pub x: i16,
    pub y: i16,
    pub width: i16,
    pub ended: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct DragMap {
    pub(crate) function: *const (),
    pub(crate) call: unsafe fn(*const (), Drag) -> Erased,
}

unsafe fn call_drag_map<M: Copy>(function: *const (), drag: Drag) -> Erased {
    let function: fn(Drag) -> M = unsafe { core::mem::transmute(function) };
    Erased::new(function(drag))
}

#[derive(Clone, Copy)]
pub(crate) struct ScrollMap {
    pub(crate) function: *const (),
    pub(crate) call: unsafe fn(*const (), i32) -> Erased,
}

unsafe fn call_scroll_map<M: Copy>(function: *const (), offset: i32) -> Erased {
    let function: fn(i32) -> M = unsafe { core::mem::transmute(function) };
    Erased::new(function(offset))
}

type ItemFn<C> = for<'r> fn(&'r C, usize, &'r Ui<<C as Component>::Msg>) -> Element<'r, <C as Component>::Msg>;

/// How a `collection` places its items: rows across its width, or cells of one
/// size with as many columns as fit at least the horizontal gap apart, the
/// width left over spread between them and at both sides. The vertical gap goes
/// between rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CollectionLayout {
    List { row_height: i16 },
    Grid { width: i16, height: i16 },
}

#[derive(Clone, Copy)]
pub(crate) struct ItemMap {
    pub(crate) function: *const (),
    pub(crate) call: unsafe fn(*const (), &dyn Any, &mut Tree, u16, usize, NodeIndex),
}

unsafe fn call_item_map<C: Component>(function: *const (), component: &dyn Any, tree: &mut Tree, owner: u16, index: usize, row: NodeIndex) {
    let Some(component) = component.downcast_ref::<C>() else {
        idf::log::error!(c"ui", "collection: the item function belongs to another component");
        return;
    };
    let function: ItemFn<C> = unsafe { core::mem::transmute(function) };
    let ui = Ui::new(tree.bump(), tree.context(owner));
    let element = function(component, index, &ui);
    tree.reconcile(owner, row, &element);
}

pub(crate) trait TaskSpec {
    fn start(&self) -> Arc<Token>;
}

struct TaskElement<'a, T, F> {
    inbox: &'a Inbox<T>,
    start: F,
}

impl<T: Send + 'static, F: Fn(Reply<T>)> TaskSpec for TaskElement<'_, T, F> {
    fn start(&self) -> Arc<Token> {
        let token = Arc::new(Token);
        (self.start)(Reply::task(Arc::downgrade(&token), self.inbox.sender()));
        token
    }
}

type DrawFn<C> = for<'a, 'f> fn(&'a C, &mut Canvas<'a, 'f>);

#[derive(Clone, Copy)]
pub(crate) struct DrawMap {
    pub(crate) function: *const (),
    pub(crate) call: for<'a> unsafe fn(*const (), &'a dyn Any, &mut Canvas<'a, '_>),
}

unsafe fn call_draw_map<'a, C: Component>(function: *const (), component: &'a dyn Any, canvas: &mut Canvas<'a, '_>) {
    let Some(component) = component.downcast_ref::<C>() else {
        idf::log::error!(c"ui", "canvas: the draw function belongs to another component");
        return;
    };
    let function: DrawFn<C> = unsafe { core::mem::transmute(function) };
    function(component, canvas);
}

struct ScreenElement<S: Child + Screen> {
    props: S::Props,
}

impl<S: Child + Screen> ChildSpec for ScreenElement<S> {
    fn host_type(&self) -> TypeId {
        TypeId::of::<S>()
    }

    fn create(&self) -> Box<dyn AnyHost> {
        screen_host(S::create(&self.props))
    }

    fn update(&self, _host: &mut dyn AnyHost) -> bool {
        false
    }

    fn event_map(&self, _parent: u16) -> Option<EventMap> {
        None
    }
}

/// A description of a node, valid during one rebuild. It lives in the rebuild's
/// memory, because a view holds many at once and the stack is small: a copy is
/// the same element, and a change to one shows in the other.
pub struct Element<'a, M: Copy> {
    data: NonNull<ElementData<'a, M>>,
}

impl<M: Copy> Clone for Element<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: Copy> Copy for Element<'_, M> {}

impl<'a, M: Copy> Deref for Element<'a, M> {
    type Target = ElementData<'a, M>;

    fn deref(&self) -> &Self::Target {
        unsafe { self.data.as_ref() }
    }
}

impl<M: Copy> DerefMut for Element<'_, M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.data.as_mut() }
    }
}

pub struct ElementData<'a, M: Copy> {
    bump: NonNull<Bump>,
    pub(crate) content: Content<'a>,
    children: NonNull<[Element<'a, M>]>,
    pub(crate) style: Style,
    pub(crate) when: &'a [(State, Prop)],
    pub(crate) inherit: State,
    pub(crate) state_source: bool,
    pub(crate) id: Option<Id>,
    part: Option<Part>,
    pub(crate) role: Role,
    pub(crate) tap: Option<Erased>,
    pub(crate) drag: Option<DragMap>,
    pub(crate) on_scroll: Option<ScrollMap>,
    pub(crate) bounce: bool,
    pub(crate) anchor: ScrollEdge,
    pub(crate) anchored: Option<Anchored>,
    pub(crate) pull: Option<Pull>,
    pub(crate) gestures: Option<GestureMap>,
    pub(crate) size: Option<SizeMap>,
    pub(crate) input: Option<InputMap>,
    pub(crate) focusable: bool,
    pub(crate) keep_focus: bool,
    pub(crate) keyboard: Option<&'static Keyboard>,
    pub(crate) input_kind: InputKind,
    pub(crate) repeat: bool,
    pub(crate) state: State,
    pub(crate) appear: Option<Presence>,
    pub(crate) task: Option<&'a dyn TaskSpec>,
    pub(crate) disappear: Option<Presence>,
    _msg: PhantomData<fn() -> M>,
}

impl<'a, M: Copy> Element<'a, M> {
    fn new(bump: NonNull<Bump>, content: Content<'a>, children: &'a mut [Element<'a, M>]) -> Self {
        let data = ElementData { bump, content, children: NonNull::from(children), style: Style::default(), when: &[], inherit: State::empty(), state_source: false, id: None, part: None, role: Role::None, tap: None, drag: None, on_scroll: None, bounce: true, anchor: ScrollEdge::Start, anchored: None, pull: None, gestures: None, size: None, input: None, focusable: false, keep_focus: false, keyboard: None, input_kind: InputKind::Text, repeat: false, state: State::empty(), appear: None, disappear: None, task: None, _msg: PhantomData };
        Self { data: unsafe { NonNull::new_unchecked(bump.as_ref().alloc(data)) } }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.content, Content::Empty)
    }

    pub(crate) fn children(&self) -> &'a [Element<'a, M>] {
        unsafe { self.children.as_ref() }
    }

    /// `id!("name")` for a node to find, or a number for one that moves
    /// among its siblings.
    pub fn id(mut self, id: impl Into<Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub(crate) fn part_of(mut self, part: Part) -> Self {
        self.part = Some(part);
        self
    }

    /// Changes each `part` of this widget. Not the inside of child components.
    pub fn part(mut self, part: Part, style: impl Fn(Self) -> Self) -> Self {
        self.style_part(part, &style);
        self
    }

    fn style_part(&mut self, part: Part, style: &impl Fn(Self) -> Self) {
        if self.part == Some(part) {
            *self = style(*self);
        }
        for child in unsafe { self.children.as_mut() } {
            child.style_part(part, style);
        }
    }

    fn prop(mut self, prop: Prop) -> Self {
        self.style.apply(prop);
        self
    }

    /// Puts `style`'s base values on this node and shares its states. Set after
    /// it, values replace them; a second style adds its base and replaces states.
    pub fn style(mut self, style: &'static StateStyle) -> Self {
        for prop in style.base {
            self.style.apply(*prop);
        }
        self.style.states = Some(style);
        match style.font {
            Some(font) => self.font(font),
            None => self,
        }
    }

    /// Changes `props` while all of `state` holds, over the shared style's same
    /// state.
    pub fn when(mut self, state: State, props: &[Prop]) -> Self {
        let old = self.when;
        let count = old.len() + props.len();
        let p = unsafe { self.bump.as_ref() }.alloc_slice::<(State, Prop)>(count, |p| unsafe {
            ptr::copy_nonoverlapping(old.as_ptr(), p, old.len());
            for (i, prop) in props.iter().enumerate() {
                p.add(old.len() + i).write((state, *prop));
            }
        });
        self.when = unsafe { core::slice::from_raw_parts(p, count) };
        self
    }

    /// Lets nodes inside take states from this one, up to the next such node.
    pub fn state_source(mut self) -> Self {
        self.state_source = true;
        self
    }

    /// Takes these states from the nearest `state_source` around this node in
    /// the same component.
    pub fn inherit_state(mut self, state: State) -> Self {
        self.inherit = state;
        self
    }

    pub fn role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }

    pub fn on_tap(mut self, msg: M) -> Self {
        self.tap = Some(Erased::new(msg));
        self
    }

    /// On a `scroll` or `list`: the offset, every time it moves. Comes as often
    /// as frames do, so rebuild only for what changes the view. Past the start
    /// it is negative, past the end above the end.
    pub fn on_scroll(mut self, on_scroll: fn(i32) -> M) -> Self {
        self.on_scroll = Some(ScrollMap { function: on_scroll as *const (), call: call_scroll_map::<M> });
        self
    }

    /// On a `scroll` or `list`: whether a drag stretches it past its ends and
    /// it springs back. On by default; content that fits never bounces.
    pub fn bounce(mut self, bounce: bool) -> Self {
        self.bounce = bounce;
        self
    }

    /// On a `scroll` or `list`: the end it starts at and keeps to while it is
    /// there, as its content grows or shrinks. `Start` by default.
    pub fn scroll_anchor(mut self, edge: ScrollEdge) -> Self {
        self.anchor = edge;
        self
    }

    /// On a `scroll` or `list`: let go `height` past its start, it sends `msg`
    /// and stays `height` open until a view gives `refreshing` false; a view
    /// giving true at the start opens it. The start stretches even without
    /// `bounce`.
    pub fn pull(mut self, height: i16, refreshing: bool, msg: M) -> Self {
        self.pull = Some(Pull { height, refreshing, msg: Erased::new(msg) });
        self
    }

    /// Draws a divider along the bottom while the `scroll` or `list` with this
    /// id in this component is away from its start.
    pub fn scroll_border(mut self, id: impl Into<Id>) -> Self {
        self.style.scroll_border = Some(id.into());
        self
    }

    /// Follows a contact from the press on, ahead of any scrolling around it.
    pub fn on_drag(mut self, on_drag: fn(Drag) -> M) -> Self {
        self.drag = Some(DragMap { function: on_drag as *const (), call: call_drag_map::<M> });
        self
    }

    /// Recognizes gestures here and on what is inside, where nothing nearer
    /// takes them. A pan is taken from a scrolling parent, the others leave it
    /// one finger; with two on the node the fingers are this node's. With
    /// `DOUBLE_TAP`, the node's own `on_tap` waits to see no second tap comes.
    pub fn gestures(mut self, gestures: Gestures, on_gesture: fn(Gesture) -> M) -> Self {
        self.gestures = Some(GestureMap { gestures, function: on_gesture as *const (), call: call_gesture_map::<M> });
        self
    }

    /// A canvas's size, once laid out and whenever it changes, before it is
    /// drawn at that size.
    pub fn on_size(mut self, on_size: fn(i16, i16) -> M) -> Self {
        self.size = Some(SizeMap { function: on_size as *const (), call: call_size_map::<M> });
        self
    }

    /// Of a navigator: with `on`, shows the bottom of the stack `width` wide on
    /// the left and the rest on the right; otherwise one pane. One stack serves
    /// both, so turning loses nothing.
    pub fn split(mut self, on: bool, width: i16) -> Self {
        if let Content::Navigator { split, .. } = &mut self.content {
            split.width = on.then_some(width);
        }
        self
    }

    /// Of a navigator: how its screens change unless a move asks otherwise,
    /// over the theme's default.
    pub fn transition(mut self, transition: Transition) -> Self {
        if let Content::Navigator { transition: own, .. } = &mut self.content {
            *own = Some(transition);
        }
        self
    }

    /// Of a split navigator: makes what the right shows while nothing is open,
    /// which is otherwise empty. Pushing from that screen keeps it to come back
    /// to; left alone it is dropped with the split, and made anew next time.
    pub fn split_detail<S: Screen>(mut self, make: fn() -> S) -> Self {
        if let Content::Navigator { split, .. } = &mut self.content {
            split.detail = Some(DetailMap { function: make as *const (), call: call_detail_map::<S> });
        }
        self
    }

    /// Takes focus on a tap and receives what is typed.
    pub fn on_input(mut self, on_input: fn(TextInput) -> M) -> Self {
        self.input = Some(InputMap { function: on_input as *const (), call: call_input_map::<M> });
        self
    }

    /// Takes focus on a tap, for `State::FOCUSED` and `cx.focus`.
    pub fn focusable(mut self) -> Self {
        self.focusable = true;
        self
    }

    /// A tap here leaves the focus where it is.
    pub fn keep_focus(mut self) -> Self {
        self.keep_focus = true;
        self
    }

    /// The on-screen keyboard for this input, over `input`.
    pub fn keyboard(mut self, keyboard: &'static Keyboard) -> Self {
        self.keyboard = Some(keyboard);
        self
    }

    /// Picks the theme's keyboard for this kind of text.
    pub fn input(mut self, kind: InputKind) -> Self {
        self.input_kind = kind;
        self
    }

    /// Shown while the text is empty. On a widget around an input, it reaches
    /// the editable text inside.
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        if let Content::Text { text, .. } = &mut self.content {
            if text.is_empty() {
                *text = placeholder;
            }
        } else {
            self.placeholder_inside(placeholder);
        }
        self
    }

    fn placeholder_inside(&mut self, placeholder: &'a str) -> bool {
        unsafe { self.children.as_mut() }.iter_mut().any(|child| match &mut child.content {
            Content::Text { text, edit: Some(_), .. } => {
                if text.is_empty() {
                    *text = placeholder;
                }
                true
            }
            _ => child.placeholder_inside(placeholder),
        })
    }

    /// The ring width of a `progress_indicator`.
    pub fn stroke(mut self, px: u8) -> Self {
        if let Content::Progress { gauge: Gauge::Ring { stroke, .. }, .. } = &mut self.content {
            *stroke = px;
        }
        self
    }

    /// Repeats `on_tap` while held.
    pub fn repeat(mut self, repeat: bool) -> Self {
        self.repeat = repeat;
        self
    }

    pub(crate) fn secure(mut self) -> Self {
        if let Content::Text { edit: Some(edit), .. } = &mut self.content {
            edit.secure = true;
        }
        self
    }

    pub(crate) fn edit(self, cursor: usize, underline: Option<(usize, usize)>) -> Self {
        let edit = Edit { cursor: cursor as u32, underline: underline.map(|(a, b)| (a as u32, b as u32)), secure: false };
        let mut element = self.with_options(|o| o.carets = true);
        if let Content::Text { edit: slot, .. } = &mut element.content {
            *slot = Some(edit);
        }
        element
    }

    pub fn width(self, px: i16) -> Self {
        self.prop(Prop::Width(px))
    }

    pub fn height(self, px: i16) -> Self {
        self.prop(Prop::Height(px))
    }

    pub fn size(self, width: i16, height: i16) -> Self {
        self.width(width).height(height)
    }

    /// A share of what the parent offers: `numerator / denominator`.
    pub fn width_fraction(self, numerator: u16, denominator: u16) -> Self {
        self.prop(Prop::WidthFraction(numerator, denominator))
    }

    pub fn height_fraction(self, numerator: u16, denominator: u16) -> Self {
        self.prop(Prop::HeightFraction(numerator, denominator))
    }

    pub fn max_width(self, px: i16) -> Self {
        self.prop(Prop::MaxWidth(px))
    }

    pub fn max_height(self, px: i16) -> Self {
        self.prop(Prop::MaxHeight(px))
    }

    /// Takes this share of what siblings overflow the main axis by, and is
    /// measured again for the size left. Text has 1, other nodes 0.
    pub fn shrink(self, weight: u8) -> Self {
        self.prop(Prop::Shrink(weight))
    }

    pub fn min_width(self, px: i16) -> Self {
        self.prop(Prop::MinWidth(px))
    }

    pub fn min_height(self, px: i16) -> Self {
        self.prop(Prop::MinHeight(px))
    }

    /// Shares the free space of the main axis by this weight.
    pub fn grow(self, weight: u8) -> Self {
        self.prop(Prop::Grow(weight))
    }

    /// Height from the width, `width`:`height`.
    pub fn aspect_ratio(self, width: u8, height: u8) -> Self {
        self.prop(Prop::Aspect(width, height))
    }

    pub fn padding(self, px: i16) -> Self {
        self.prop(Prop::Padding(px))
    }

    pub fn padding_x(self, px: i16) -> Self {
        self.prop(Prop::PaddingX(px))
    }

    pub fn padding_y(self, px: i16) -> Self {
        self.prop(Prop::PaddingY(px))
    }

    /// Left, top, right, bottom.
    pub fn padding_each(self, left: i16, top: i16, right: i16, bottom: i16) -> Self {
        self.prop(Prop::PaddingEach(left, top, right, bottom))
    }

    pub fn gap(self, px: i16) -> Self {
        self.prop(Prop::Gap(px))
    }

    pub fn gap_xy(self, x: i16, y: i16) -> Self {
        self.prop(Prop::GapX(x)).prop(Prop::GapY(y))
    }

    /// Children on the cross axis.
    pub fn align(self, align: Align) -> Self {
        self.prop(Prop::Align(align))
    }

    pub fn align_self(self, align: Align) -> Self {
        self.prop(Prop::AlignSelf(align))
    }

    pub fn justify(self, justify: Justify) -> Self {
        self.prop(Prop::Justify(justify))
    }

    /// Takes the node out of the flow, placed by distances from the parent.
    pub(crate) fn anchored(mut self, anchored: Anchored) -> Self {
        self.anchored = Some(anchored);
        self.style.absolute = Some(Inset::default());
        self
    }

    pub fn absolute(mut self, left: Option<i16>, top: Option<i16>, right: Option<i16>, bottom: Option<i16>) -> Self {
        self.style.absolute = Some(Inset { left, top, right, bottom });
        self
    }

    pub fn background(self, color: impl Into<ColorRole>) -> Self {
        self.prop(Prop::Background(color.into()))
    }

    /// The color of text and icons here and inside, where they set none of
    /// their own.
    pub fn foreground(self, color: impl Into<ColorRole>) -> Self {
        self.prop(Prop::Foreground(color.into()))
    }

    /// Takes no input, and looks it.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.state.set(State::DISABLED, disabled);
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.state.set(State::SELECTED, selected);
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.state.set(State::CHECKED, checked);
        self
    }

    pub fn radius(self, px: u8) -> Self {
        self.prop(Prop::Radius(px))
    }

    /// Which corners `radius` rounds; the others stay square.
    pub fn corners(self, corners: Corners) -> Self {
        self.prop(Prop::Corners(corners))
    }

    /// An outline inside the box, following its radius.
    pub fn border(self, color: impl Into<ColorRole>, width: u8) -> Self {
        self.prop(Prop::Border(color.into(), width))
    }

    /// Which sides `border` runs along. A rounded corner with only one of its
    /// sides ends that side where the curve starts.
    pub fn border_sides(self, sides: Sides) -> Self {
        self.prop(Prop::BorderSides(sides))
    }

    pub fn shadow(mut self, shadow: Shadow) -> Self {
        self.style.shadow = Some(shadow);
        self
    }

    pub fn opacity(self, opacity: u8) -> Self {
        self.prop(Prop::Opacity(opacity))
    }

    /// Eases what `animated` names when the style changes to this one.
    pub fn animate(self, animated: Animated, ms: u16) -> Self {
        self.prop(Prop::Animate(animated, ms))
    }

    /// Comes in from `presence` once placed. Without `disappear`, goes the
    /// same way backwards.
    pub fn appear(mut self, presence: Presence) -> Self {
        self.appear = Some(presence);
        self
    }

    /// Goes to `presence` when taken out of the view, or with an `Overlay`'s
    /// screen.
    pub fn disappear(mut self, presence: Presence) -> Self {
        self.disappear = Some(presence);
        self
    }

    /// Clips what is inside, to the `radius` too. The inside of a scroller or
    /// a surface shown by the display stays square.
    pub fn clip(self) -> Self {
        self.prop(Prop::Clip(true))
    }

    /// Items are built again only when this changes, instead of on every
    /// rebuild of the owner. Change it whenever items would look different.
    pub fn revision(mut self, revision: u32) -> Self {
        if let Content::Collection { revision: r, .. } = &mut self.content {
            *r = Some(revision);
        }
        self
    }

    /// How many rows exist past each end of what shows, 1 by default.
    pub fn overscan(mut self, rows: u8) -> Self {
        if let Content::Collection { overscan, .. } = &mut self.content {
            *overscan = rows;
        }
        self
    }

    /// Calls `start` once this node is made, and cancels its `Reply` once the
    /// node goes: out of a collection's rows, with its screen, or without the task in
    /// the view. Values come through `inbox`. `start` is never dropped, so it
    /// may hold only what needs no drop.
    pub fn task<T: Send + 'static, F: Fn(Reply<T>) + 'a>(mut self, inbox: &'a Inbox<T>, start: F) -> Self {
        const { assert!(!core::mem::needs_drop::<F>(), "a task's start must need no drop") };
        let spec = unsafe { self.bump.as_ref() }.alloc(TaskElement { inbox, start });
        self.task = Some(unsafe { &*spec });
        self
    }

    pub fn font(mut self, font: &'static FontChain) -> Self {
        if let Content::Text { font: f, .. } = &mut self.content {
            *f = font;
        }
        self
    }

    pub(crate) fn font_px(&self) -> i16 {
        match self.content {
            Content::Text { font, .. } => font.px() as i16,
            _ => 0,
        }
    }

    fn with_options(mut self, f: impl FnOnce(&mut TextOptions)) -> Self {
        if let Content::Text { options, .. } = &mut self.content {
            f(options);
        }
        self
    }

    /// Replaces what does not fit with an ellipsis; not for editable text.
    pub fn ellipsis(self, elide: Elide) -> Self {
        self.with_options(|o| o.overflow = Overflow::Ellipsis(elide))
    }

    pub fn wrap(self) -> Self {
        self.with_options(|o| o.wrap = true)
    }

    pub fn max_lines(self, lines: u16) -> Self {
        self.with_options(|o| o.max_lines = lines)
    }

    pub fn text_center(self) -> Self {
        self.with_options(|o| o.align = TextAlign::Center)
    }

    pub fn text_end(self) -> Self {
        self.with_options(|o| o.align = TextAlign::End)
    }
}

/// Children of a container, moved into the rebuild's memory.
pub trait Children<'a, M: Copy> {
    fn place(self, ui: &'a Ui<M>) -> &'a mut [Element<'a, M>];
}

impl<'a, M: Copy, const N: usize> Children<'a, M> for [Element<'a, M>; N] {
    fn place(self, ui: &'a Ui<M>) -> &'a mut [Element<'a, M>] {
        ui.place_slice(&self)
    }
}

impl<'a, M: Copy> Children<'a, M> for Vec<Element<'a, M>> {
    fn place(self, ui: &'a Ui<M>) -> &'a mut [Element<'a, M>] {
        ui.place_slice(&self)
    }
}

/// What a view can read about where it is shown.
#[derive(Clone, Copy)]
pub(crate) struct BuildContext {
    pub(crate) theme: &'static Theme,
    pub(crate) insets: Insets,
    pub(crate) area: devkit_gfx::Rect,
    pub(crate) screen_style: ScreenStyle,
    pub(crate) format: ImageFormat,
    pub(crate) split_role: SplitRole,
    pub(crate) can_pop: bool,
    pub(crate) colors: Palette,
    pub(crate) color_mode: ColorMode,
}

/// Builds elements; everything it hands out lives for the rebuild.
pub struct Ui<M> {
    bump: NonNull<Bump>,
    context: BuildContext,
    _msg: PhantomData<fn(M)>,
}

impl<M: Copy> Ui<M> {
    pub(crate) fn new(bump: NonNull<Bump>, context: BuildContext) -> Self {
        Self { bump, context, _msg: PhantomData }
    }

    pub fn theme(&self) -> &'static Theme {
        self.context.theme
    }

    /// The space the chrome covers.
    pub fn color_mode(&self) -> ColorMode {
        self.context.color_mode
    }

    /// The theme's colors in the color mode set.
    pub fn colors(&self) -> Palette {
        self.context.colors
    }

    pub fn color(&self, role: ColorRole) -> Color {
        self.context.colors.color(role)
    }

    /// Whether this screen's navigator has somewhere to go back to from here.
    /// Next to a main screen, the first screen on the right has not.
    pub fn can_pop(&self) -> bool {
        self.context.can_pop
    }

    pub fn split_role(&self) -> SplitRole {
        self.context.split_role
    }

    /// The format `Image::from_pixels` copies without conversion.
    pub fn pixel_format(&self) -> ImageFormat {
        self.context.format
    }

    pub fn insets(&self) -> Insets {
        self.context.insets
    }

    /// The logical size of the whole UI.
    pub fn screen_size(&self) -> (i16, i16) {
        (self.context.area.w, self.context.area.h)
    }

    /// The style of the screen on top of the root navigator, for the chrome.
    pub fn screen_style(&self) -> ScreenStyle {
        self.context.screen_style
    }

    fn bump(&self) -> &Bump {
        unsafe { self.bump.as_ref() }
    }

    fn place_slice<'a>(&'a self, items: &[Element<'a, M>]) -> &'a mut [Element<'a, M>] {
        let p = self.bump().alloc_slice::<Element<'a, M>>(items.len(), |p| unsafe {
            ptr::copy_nonoverlapping(items.as_ptr(), p, items.len());
        });
        unsafe { core::slice::from_raw_parts_mut(p, items.len()) }
    }

    /// Nothing, for a conditional child.
    pub fn empty<'a>(&'a self) -> Element<'a, M> {
        Element::new(self.bump, Content::Empty, &mut [])
    }

    fn container<'a>(&'a self, direction: Direction, children: impl Children<'a, M>) -> Element<'a, M> {
        let mut element = Element::new(self.bump, Content::Box, children.place(self));
        element.style.direction = direction;
        element
    }

    pub fn column<'a>(&'a self, children: impl Children<'a, M>) -> Element<'a, M> {
        self.container(Direction::Column, children)
    }

    pub fn row<'a>(&'a self, children: impl Children<'a, M>) -> Element<'a, M> {
        self.container(Direction::Row, children)
    }

    pub fn grid<'a>(&'a self, columns: u8, children: impl Children<'a, M>) -> Element<'a, M> {
        self.container(Direction::Grid(columns), children)
    }

    pub fn text<'a>(&'a self, font: &'static FontChain, text: &'a str) -> Element<'a, M> {
        let options = TextOptions::default();
        let mut element = Element::new(self.bump, Content::Text { font, text, options, edit: None }, &mut []);
        element.role = Role::Text;
        element.style.shrink = 1;
        element
    }

    pub fn icon<'a>(&'a self, icon: Icon) -> Element<'a, M> {
        let mut buffer = [0; 4];
        self.text(icon.font, self.store(icon.glyph.encode_utf8(&mut buffer)))
    }

    /// Copies text into the rebuild's memory.
    pub fn store<'a>(&'a self, text: &str) -> &'a str {
        unsafe { &*self.bump().alloc_str(text) }
    }

    pub fn format<'a>(&'a self, args: fmt::Arguments<'_>) -> &'a str {
        let mut text = alloc::string::String::new();
        let _ = text.write_fmt(args);
        self.store(&text)
    }

    pub fn image<'a>(&'a self, image: Image<'static>) -> Element<'a, M> {
        Element::new(self.bump, Content::Image { image, tint: Color::rgb(0, 0, 0) }, &mut [])
    }

    /// Pixels drawn by `draw` of the component whose view this is, in every
    /// frame that shows it. Takes the size the parent gives it.
    pub fn canvas<'a, C: Component<Msg = M>>(&'a self, draw: DrawFn<C>) -> Element<'a, M> {
        let draw = DrawMap { function: draw as *const (), call: call_draw_map::<C> };
        Element::new(self.bump, Content::Canvas(draw), &mut [])
    }

    pub(crate) fn progress<'a>(&'a self, gauge: Gauge, value: Option<f32>, period_ms: u16) -> Element<'a, M> {
        Element::new(self.bump, Content::Progress { gauge, value, period_ms }, &mut [])
    }

    /// A rectangle `surface` writes, inside the size the parent gives it: its
    /// top and bottom keep to the 16 row grid damage is aligned to, so it can
    /// end up to 15 rows shorter on each side. One at a time per app.
    pub fn external<'a>(&'a self, surface: &'a ExternalSurface) -> Element<'a, M> {
        Element::new(self.bump, Content::External(surface), &mut [])
    }

    /// Scrolls one child vertically. As tall as the child where nothing sets
    /// its height, up to what the parent or `max_height` allows, and shrinks
    /// like text.
    pub fn scroll<'a>(&'a self, child: Element<'a, M>) -> Element<'a, M> {
        let mut element = Element::new(self.bump, Content::Scroll, self.place_slice(&[child]));
        element.style.shrink = 1;
        element
    }

    /// `count` items placed by `layout`, scrolling vertically. Only the rows in
    /// view exist, each item made by `item` of the component whose view this
    /// is. Padding scrolls with the items. One with an id keeps its scroll
    /// position while its screen is covered.
    pub fn collection<'a, C: Component<Msg = M>>(&'a self, count: usize, layout: CollectionLayout, item: ItemFn<C>) -> Element<'a, M> {
        let item = ItemMap { function: item as *const (), call: call_item_map::<C> };
        let mut element = Element::new(self.bump, Content::Collection { count, layout, revision: None, overscan: 1, item }, &mut []);
        element.role = Role::Collection;
        element
    }

    /// A navigator marked by `K`, starting with screen `S`. It takes the space
    /// the parent gives it and keeps its stack while it stays in the tree.
    pub fn navigator<'a, K: 'static, S: Child + Screen>(&'a self, props: S::Props) -> Element<'a, M> {
        let spec = self.bump().alloc(ScreenElement::<S> { props });
        let spec: &'a ScreenElement<S> = unsafe { &*spec };
        Element::new(self.bump, Content::Navigator { key: TypeId::of::<K>(), initial: Some(spec), split: SplitSpec::default(), transition: None }, &mut []).clip()
    }

    /// A child component, kept across rebuilds by position or id.
    pub fn child<'a, C: Child>(&'a self, props: C::Props) -> Element<'a, M>
    where
        M: 'static,
    {
        self.child_with::<C>(props, None)
    }

    /// The same, turning the child's events into this component's messages.
    pub fn child_mapped<'a, C: Child>(&'a self, props: C::Props, on_event: fn(C::Event) -> M) -> Element<'a, M>
    where
        M: 'static,
    {
        self.child_with::<C>(props, Some(on_event))
    }

    fn child_with<'a, C: Child>(&'a self, props: C::Props, on_event: Option<fn(C::Event) -> M>) -> Element<'a, M>
    where
        M: 'static,
    {
        let spec = self.bump().alloc(ChildElement::<C, M> { props, on_event });
        let spec: &'a ChildElement<C, M> = unsafe { &*spec };
        Element::new(self.bump, Content::Child(spec), &mut [])
    }
}
