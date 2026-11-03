/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::Cell;

use devkit_gfx::{Color, FontChain, Image, Rect, TextOptions, TextRun};
use idf::heap::Caps;

use crate::animate::Fades;
use crate::external::ExternalSurface;
use crate::element::{DragMap, DrawMap, Edit, InputMap, RowMap, ScrollMap, SizeMap};
use crate::erased::Erased;
use crate::gesture::GestureMap;
use crate::id::Id;
use crate::keyboard::{InputKind, Keyboard};
use crate::nav::NavigatorNode;
use crate::presence::{Pose, Presence};
use crate::progress::ProgressNode;
use crate::style::{Prop, Role, State, Style};

const CARET_WIDTH: i16 = 2;

pub(crate) type NodeIndex = u32;
pub(crate) const NONE: NodeIndex = u32::MAX;

pub(crate) struct TextNode {
    pub(crate) font: &'static FontChain,
    pub(crate) string: Vec<u8, Caps>,
    pub(crate) options: TextOptions,
    pub(crate) run: TextRun<Caps>,
    /// The width `run` was laid out for; `None` until it is.
    pub(crate) laid_out: Option<i16>,
    pub(crate) edit: Option<Edit>,
    pub(crate) scroll: Cell<i16>,
}

impl Node {
    pub(crate) fn scroll_offset_mut(&mut self) -> Option<&mut i32> {
        match &mut self.content {
            NodeContent::Scroll(scroll) => Some(&mut scroll.offset),
            NodeContent::List(list) => Some(&mut list.offset),
            _ => None,
        }
    }

    pub(crate) fn scroll_offset(&self) -> Option<i32> {
        match &self.content {
            NodeContent::Scroll(scroll) => Some(scroll.offset),
            NodeContent::List(list) => Some(list.offset),
            _ => None,
        }
    }
}

impl TextNode {
    pub(crate) fn text(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.string) }
    }

    /// How far a single line moves left, which changes only as far as it
    /// takes to keep the cursor in `width`.
    pub(crate) fn scroll(&self, width: i16) -> i16 {
        let Some(edit) = &self.edit else { return 0 };
        if self.options.wrap {
            return 0;
        }
        let (x, _) = self.run.caret(edit.cursor as usize).unwrap_or((0, 0));
        let offset = self.scroll.get().min(x).max(x + CARET_WIDTH - width);
        let offset = offset.min(self.run.width() + CARET_WIDTH - width).max(0);
        self.scroll.set(offset);
        offset
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Pull {
    pub(crate) height: i16,
    pub(crate) refreshing: bool,
    pub(crate) msg: Erased,
}

pub(crate) struct ScrollNode {
    pub(crate) offset: i32,
    pub(crate) content: i32,
}

pub(crate) struct ListNode {
    pub(crate) row: RowMap,
    pub(crate) overscan: u8,
    pub(crate) count: usize,
    pub(crate) row_height: i16,
    pub(crate) revision: Option<u32>,
    pub(crate) offset: i32,
    /// Row index and node of what exists, in row order.
    pub(crate) rows: Vec<(u32, NodeIndex), Caps>,
    /// The owner's view ran, so the rows must be built again.
    pub(crate) stale: bool,
}

pub(crate) struct CanvasNode {
    pub(crate) draw: DrawMap,
    pub(crate) reported: (i16, i16),
}

pub(crate) enum NodeContent {
    Box,
    Text(TextNode),
    Image { image: Image<'static>, tint: Color },
    Scroll(ScrollNode),
    List(ListNode),
    Canvas(CanvasNode),
    External(ExternalSurface),
    Progress(ProgressNode),
    Child(u16),
    Navigator(NavigatorNode),
}

impl NodeContent {
    pub(crate) fn same_kind(&self, other: &crate::element::Content<'_>) -> bool {
        use crate::element::Content as C;
        matches!(
            (self, other),
            (Self::Box, C::Box)
                | (Self::Text(_), C::Text { .. })
                | (Self::Image { .. }, C::Image { .. })
                | (Self::Scroll(_), C::Scroll)
                | (Self::List(_), C::List { .. })
                | (Self::Canvas(_), C::Canvas(_))
                | (Self::External(_), C::External(_))
                | (Self::Progress(_), C::Progress { .. })
        ) || matches!((self, other), (Self::Navigator(nav), C::Navigator { key, .. }) if nav.key == *key)
    }
}

pub(crate) struct Node {
    pub(crate) content: NodeContent,
    /// With what its state selects applied.
    pub(crate) style: Style,
    /// `style` before that, while it differs.
    pub(crate) base: Option<Box<Style, Caps>>,
    pub(crate) when: Vec<(State, Prop), Caps>,
    pub(crate) parent: NodeIndex,
    pub(crate) first_child: NodeIndex,
    pub(crate) next: NodeIndex,
    pub(crate) id: Option<Id>,
    pub(crate) role: Role,
    pub(crate) owner: u16,
    pub(crate) tap: Option<Erased>,
    pub(crate) drag: Option<DragMap>,
    pub(crate) on_scroll: Option<ScrollMap>,
    pub(crate) bounce: bool,
    pub(crate) pull: Option<Pull>,
    pub(crate) gestures: Option<GestureMap>,
    pub(crate) size: Option<SizeMap>,
    pub(crate) input: Option<InputMap>,
    pub(crate) focusable: bool,
    pub(crate) keep_focus: bool,
    pub(crate) keyboard: Option<&'static Keyboard>,
    pub(crate) input_kind: InputKind,
    pub(crate) repeat: bool,
    /// Moves the node and what is in it, without layout.
    pub(crate) translate: (i16, i16),
    /// An animated opacity, over the style's.
    pub(crate) opacity: Option<u8>,
    pub(crate) fades: Option<Box<Fades, Caps>>,
    pub(crate) appear: Option<Presence>,
    pub(crate) disappear: Option<Presence>,
    pub(crate) pose: Option<Pose>,
    /// Out of the view, kept until its disappearance ends.
    pub(crate) leaving: bool,
    /// Has a token in `Tree::tasks`.
    pub(crate) task: bool,
    /// Relative to the parent's origin.
    pub(crate) rect: Rect,
    /// Where it was last painted.
    pub(crate) screen: Rect,
    pub(crate) paint_dirty: bool,
    pub(crate) state: State,
    /// The states set on this node, and those taken from its `state_source`.
    pub(crate) own_state: State,
    pub(crate) source_state: State,
    pub(crate) inherit: State,
    pub(crate) state_source: bool,
    /// The main-axis size the parent's flow gave it, and whether that differs
    /// from what it asked for.
    pub(crate) flow_size: i16,
    pub(crate) flow_resized: bool,
    /// Scrolled since the last frame; its viewport is already damaged.
    pub(crate) scrolled: bool,
    pub(crate) measured: Option<(i16, i16, i16, i16)>,
}

impl Node {
    /// Where it is moved from its place, without layout.
    pub(crate) fn shift(&self) -> (i16, i16) {
        let (x, y) = self.pose.map_or((0, 0), |p| p.offset);
        (self.translate.0 + x, self.translate.1 + y)
    }

    pub(crate) fn new(content: NodeContent, owner: u16, caps: Caps) -> Self {
        Self {
            content,
            style: Style::default(),
            base: None,
            when: Vec::new_in(caps),
            parent: NONE,
            first_child: NONE,
            next: NONE,
            id: None,
            role: Role::None,
            owner,
            tap: None,
            drag: None,
            on_scroll: None,
            bounce: true,
            pull: None,
            gestures: None,
            size: None,
            input: None,
            focusable: false,
            keep_focus: false,
            keyboard: None,
            input_kind: InputKind::Text,
            repeat: false,
            translate: (0, 0),
            opacity: None,
            fades: None,
            appear: None,
            disappear: None,
            pose: None,
            leaving: false,
            task: false,
            rect: Rect::default(),
            screen: Rect::default(),
            paint_dirty: true,
            state: State::empty(),
            own_state: State::empty(),
            source_state: State::empty(),
            inherit: State::empty(),
            state_source: false,
            flow_size: 0,
            flow_resized: false,
            scrolled: false,
            measured: None,
        }
    }
}
