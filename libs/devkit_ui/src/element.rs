/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::TypeId;
use core::fmt::{self, Write};
use core::marker::PhantomData;
use core::ptr::{self, NonNull};

use devkit_gfx::{Align as TextAlign, Color, FontChain, Image, ImageFormat, Overflow, TextOptions};

use crate::bump::Bump;
use crate::component::{AnyHost, Child, EventMap, Host, ListId, Screen, call_event_map, screen_host};
use crate::erased::Erased;
use crate::style::{Align, Dimension, Direction, Inset, Insets, Justify, Role, ScreenStyle, Shadow, State, Style};
use crate::canvas::CanvasId;
use crate::external::ExternalSurface;
use crate::gesture::{Gesture, GestureMap, Gestures, call_gesture_map};
use crate::keyboard::{InputKind, Keyboard};
use crate::text_buffer::TextInput;
use crate::theme::{Border, Icon, Theme, WidgetLook};

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
        Box::new(Host { component: C::create(&self.props), props: Some(self.props) })
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
    /// Without a color, the foreground of the nearest look, or the theme's.
    Text { font: &'static FontChain, text: &'a str, color: Option<Color>, options: TextOptions, edit: Option<Edit> },
    Image { image: Image<'static>, tint: Color },
    Scroll,
    List { id: ListId, count: usize, row_height: i16, revision: Option<u32> },
    Canvas(CanvasId),
    External(&'a ExternalSurface),
    Child(&'a dyn ChildSpec),
    Navigator { key: TypeId, initial: Option<&'a dyn ChildSpec> },
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

/// A drag on a node, in the node's coordinates.
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

/// A description of a node, valid during one rebuild.
#[derive(Clone, Copy)]
pub struct Element<'a, M: Copy> {
    pub(crate) content: Content<'a>,
    pub(crate) children: &'a [Element<'a, M>],
    pub(crate) style: Style,
    pub(crate) key: Option<u32>,
    pub(crate) id: Option<&'static str>,
    pub(crate) role: Role,
    pub(crate) tap: Option<Erased>,
    pub(crate) drag: Option<DragMap>,
    pub(crate) gestures: Option<GestureMap>,
    pub(crate) size: Option<SizeMap>,
    pub(crate) input: Option<InputMap>,
    pub(crate) focusable: bool,
    pub(crate) keep_focus: bool,
    pub(crate) keyboard: Option<&'static Keyboard>,
    pub(crate) input_kind: InputKind,
    pub(crate) repeat: bool,
    pub(crate) state: State,
    _msg: PhantomData<fn() -> M>,
}

impl<'a, M: Copy> Element<'a, M> {
    fn new(content: Content<'a>, children: &'a [Element<'a, M>]) -> Self {
        Self { content, children, style: Style::default(), key: None, id: None, role: Role::None, tap: None, drag: None, gestures: None, size: None, input: None, focusable: false, keep_focus: false, keyboard: None, input_kind: InputKind::Text, repeat: false, state: State::empty(), _msg: PhantomData }
    }

    pub const fn is_empty(&self) -> bool {
        matches!(self.content, Content::Empty)
    }

    /// Matches this node across rebuilds when siblings move.
    pub fn key(mut self, key: u32) -> Self {
        self.key = Some(key);
        self
    }

    /// Names the node for `ui tree`, `ui tap` and `ui invoke`.
    pub fn id(mut self, id: &'static str) -> Self {
        self.id = Some(id);
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

    /// Shown while the text is empty.
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        if let Content::Text { text, .. } = &mut self.content
            && text.is_empty()
        {
            *text = placeholder;
        }
        self
    }

    /// Repeats `on_tap` while held.
    pub fn repeat(mut self, repeat: bool) -> Self {
        self.repeat = repeat;
        self
    }

    pub(crate) fn edit(self, cursor: usize, underline: Option<(usize, usize)>) -> Self {
        let edit = Edit { cursor: cursor as u32, underline: underline.map(|(a, b)| (a as u32, b as u32)) };
        let mut element = self.with_options(|o| o.carets = true);
        if let Content::Text { edit: slot, .. } = &mut element.content {
            *slot = Some(edit);
        }
        element
    }

    pub fn width(mut self, px: i16) -> Self {
        self.style.width = Dimension::Px(px);
        self
    }

    pub fn height(mut self, px: i16) -> Self {
        self.style.height = Dimension::Px(px);
        self
    }

    pub fn size(self, width: i16, height: i16) -> Self {
        self.width(width).height(height)
    }

    /// A share of what the parent offers: `numerator / denominator`.
    pub fn width_fraction(mut self, numerator: u16, denominator: u16) -> Self {
        self.style.width = Dimension::Fraction(numerator, denominator.max(1));
        self
    }

    pub fn height_fraction(mut self, numerator: u16, denominator: u16) -> Self {
        self.style.height = Dimension::Fraction(numerator, denominator.max(1));
        self
    }

    pub fn max_width(mut self, px: i16) -> Self {
        self.style.max_width = px;
        self
    }

    pub fn max_height(mut self, px: i16) -> Self {
        self.style.max_height = px;
        self
    }

    /// Takes this share of what siblings overflow the main axis by, and is
    /// measured again for the size left. Text has 1, other nodes 0.
    pub fn shrink(mut self, weight: u8) -> Self {
        self.style.shrink = weight;
        self
    }

    pub fn min_width(mut self, px: i16) -> Self {
        self.style.min_width = px;
        self
    }

    pub fn min_height(mut self, px: i16) -> Self {
        self.style.min_height = px;
        self
    }

    /// Shares the free space of the main axis by this weight.
    pub fn grow(mut self, weight: u8) -> Self {
        self.style.grow = weight;
        self
    }

    /// Height from the width, `width`:`height`.
    pub fn aspect_ratio(mut self, width: u8, height: u8) -> Self {
        self.style.aspect = Some((width, height));
        self
    }

    pub fn padding(self, px: i16) -> Self {
        self.padding_x(px).padding_y(px)
    }

    pub fn padding_x(mut self, px: i16) -> Self {
        self.style.padding[0] = px;
        self.style.padding[2] = px;
        self
    }

    pub fn padding_y(mut self, px: i16) -> Self {
        self.style.padding[1] = px;
        self.style.padding[3] = px;
        self
    }

    /// Left, top, right, bottom.
    pub fn padding_each(mut self, left: i16, top: i16, right: i16, bottom: i16) -> Self {
        self.style.padding = [left, top, right, bottom];
        self
    }

    pub fn gap(mut self, px: i16) -> Self {
        self.style.gap_x = px;
        self.style.gap_y = px;
        self
    }

    pub fn gap_xy(mut self, x: i16, y: i16) -> Self {
        self.style.gap_x = x;
        self.style.gap_y = y;
        self
    }

    /// Children on the cross axis.
    pub fn align(mut self, align: Align) -> Self {
        self.style.align = align;
        self
    }

    pub fn align_self(mut self, align: Align) -> Self {
        self.style.align_self = Some(align);
        self
    }

    pub fn justify(mut self, justify: Justify) -> Self {
        self.style.justify = justify;
        self
    }

    /// Takes the node out of the flow, placed by distances from the parent.
    pub fn absolute(mut self, left: Option<i16>, top: Option<i16>, right: Option<i16>, bottom: Option<i16>) -> Self {
        self.style.absolute = Some(Inset { left, top, right, bottom });
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.style.background = Some(color);
        self
    }

    /// Appearance by state.
    pub fn look(mut self, look: &'static WidgetLook) -> Self {
        self.style.look = Some(look);
        self
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

    pub fn radius(mut self, px: u8) -> Self {
        self.style.radius = px;
        self
    }

    /// An outline inside the box, following its radius.
    pub fn border(mut self, color: Color, width: u8) -> Self {
        self.style.border = Some(Border { color, width });
        self
    }

    pub fn border_top(mut self, color: Color) -> Self {
        self.style.border_top = Some(color);
        self
    }

    pub fn border_bottom(mut self, color: Color) -> Self {
        self.style.border_bottom = Some(color);
        self
    }

    pub fn shadow(mut self, shadow: Shadow) -> Self {
        self.style.shadow = Some(shadow);
        self
    }

    pub fn opacity(mut self, opacity: u8) -> Self {
        self.style.opacity = Some(opacity);
        self
    }

    pub fn clip(mut self) -> Self {
        self.style.clip = true;
        self
    }

    /// Rows are built again only when this changes, instead of on every
    /// rebuild of the owner. Change it whenever rows would look different.
    pub fn revision(mut self, revision: u32) -> Self {
        if let Content::List { revision: r, .. } = &mut self.content {
            *r = Some(revision);
        }
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        if let Content::Text { color: c, .. } = &mut self.content {
            *c = Some(color);
        }
        self
    }

    fn with_options(mut self, f: impl FnOnce(&mut TextOptions)) -> Self {
        if let Content::Text { options, .. } = &mut self.content {
            f(options);
        }
        self
    }

    pub fn ellipsis(self) -> Self {
        self.with_options(|o| o.overflow = Overflow::Ellipsis)
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
    fn place(self, ui: &'a Ui<M>) -> &'a [Element<'a, M>];
}

impl<'a, M: Copy, const N: usize> Children<'a, M> for [Element<'a, M>; N] {
    fn place(self, ui: &'a Ui<M>) -> &'a [Element<'a, M>] {
        ui.place_slice(&self)
    }
}

impl<'a, M: Copy> Children<'a, M> for Vec<Element<'a, M>> {
    fn place(self, ui: &'a Ui<M>) -> &'a [Element<'a, M>] {
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

    fn place_slice<'a>(&'a self, items: &[Element<'a, M>]) -> &'a [Element<'a, M>] {
        let p = self.bump().alloc_slice::<Element<'a, M>>(items.len(), |p| unsafe {
            ptr::copy_nonoverlapping(items.as_ptr(), p, items.len());
        });
        unsafe { core::slice::from_raw_parts(p, items.len()) }
    }

    /// Nothing, for a conditional child.
    pub fn empty<'a>(&'a self) -> Element<'a, M> {
        Element::new(Content::Empty, &[])
    }

    fn container<'a>(&'a self, direction: Direction, children: impl Children<'a, M>) -> Element<'a, M> {
        let mut element = Element::new(Content::Box, children.place(self));
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
        let mut element = Element::new(Content::Text { font, text, color: None, options, edit: None }, &[]);
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
        Element::new(Content::Image { image, tint: Color::rgb(0, 0, 0) }, &[])
    }

    /// Pixels drawn by `Component::draw`. Takes the size the parent gives it.
    pub fn canvas<'a>(&'a self, id: CanvasId) -> Element<'a, M> {
        Element::new(Content::Canvas(id), &[])
    }

    /// A rectangle `surface` writes, inside the size the parent gives it: its
    /// top and bottom keep to the 16 row grid damage is aligned to, so it can
    /// end up to 15 rows shorter on each side. One at a time per app.
    pub fn external<'a>(&'a self, surface: &'a ExternalSurface) -> Element<'a, M> {
        Element::new(Content::External(surface), &[])
    }

    /// Scrolls one child vertically. Takes the size the parent gives it.
    pub fn scroll<'a>(&'a self, child: Element<'a, M>) -> Element<'a, M> {
        Element::new(Content::Scroll, self.place_slice(&[child]))
    }

    /// Rows come from `Component::list_row`; only visible ones exist.
    pub fn list<'a>(&'a self, id: ListId, count: usize, row_height: i16) -> Element<'a, M> {
        let mut element = Element::new(Content::List { id, count, row_height, revision: None }, &[]);
        element.role = Role::List;
        element
    }

    /// A navigator marked by `K`, starting with screen `S`. It takes the space
    /// the parent gives it and keeps its stack while it stays in the tree.
    pub fn navigator<'a, K: 'static, S: Child + Screen>(&'a self, props: S::Props) -> Element<'a, M> {
        let spec = self.bump().alloc(ScreenElement::<S> { props });
        let spec: &'a ScreenElement<S> = unsafe { &*spec };
        Element::new(Content::Navigator { key: TypeId::of::<K>(), initial: Some(spec) }, &[]).clip()
    }

    /// A child component, kept across rebuilds by position or key.
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
        Element::new(Content::Child(spec), &[])
    }
}
