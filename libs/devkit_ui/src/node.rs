/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;

use devkit_gfx::{Color, FontChain, Image, Rect, TextOptions, TextRun};
use idf::heap::Caps;

use crate::component::ListId;
use crate::element::DragMap;
use crate::erased::Erased;
use crate::nav::NavigatorNode;
use crate::style::{Role, State, Style};

pub(crate) type NodeId = u32;
pub(crate) const NONE: NodeId = u32::MAX;

pub(crate) struct TextNode {
    pub(crate) font: &'static FontChain,
    pub(crate) string: Vec<u8, Caps>,
    pub(crate) color: Option<Color>,
    pub(crate) options: TextOptions,
    pub(crate) run: TextRun<Caps>,
    /// The width `run` was laid out for; `None` until it is.
    pub(crate) laid_out: Option<i16>,
}

impl TextNode {
    pub(crate) fn text(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.string) }
    }
}

pub(crate) struct ScrollNode {
    pub(crate) offset: i32,
    pub(crate) content: i32,
}

pub(crate) struct ListNode {
    pub(crate) id: ListId,
    pub(crate) count: usize,
    pub(crate) row_height: i16,
    pub(crate) revision: Option<u32>,
    pub(crate) offset: i32,
    /// Row index and node of what exists, in row order.
    pub(crate) rows: Vec<(u32, NodeId), Caps>,
    /// The owner's view ran, so the rows must be built again.
    pub(crate) stale: bool,
}

pub(crate) enum NodeContent {
    Box,
    Text(TextNode),
    Image { image: Image<'static>, tint: Color },
    Scroll(ScrollNode),
    List(ListNode),
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
        ) || matches!((self, other), (Self::Navigator(nav), C::Navigator { key, .. }) if nav.key == *key)
    }
}

pub(crate) struct Node {
    pub(crate) content: NodeContent,
    pub(crate) style: Style,
    pub(crate) parent: NodeId,
    pub(crate) first_child: NodeId,
    pub(crate) next: NodeId,
    pub(crate) key: Option<u32>,
    pub(crate) id: Option<&'static str>,
    pub(crate) role: Role,
    pub(crate) owner: u16,
    pub(crate) tap: Option<Erased>,
    pub(crate) drag: Option<DragMap>,
    /// Moves the node and what is in it, without layout.
    pub(crate) translate: (i16, i16),
    /// An animated opacity, over the style's.
    pub(crate) opacity: Option<u8>,
    /// Relative to the parent's origin.
    pub(crate) rect: Rect,
    /// Where it was last painted.
    pub(crate) screen: Rect,
    pub(crate) paint_dirty: bool,
    pub(crate) state: State,
    /// The main-axis size the parent's flow gave it, and whether that differs
    /// from what it asked for.
    pub(crate) flow_size: i16,
    pub(crate) flow_resized: bool,
    /// Scrolled since the last frame; its viewport is already damaged.
    pub(crate) scrolled: bool,
    pub(crate) measured: Option<(i16, i16, i16, i16)>,
}

impl Node {
    pub(crate) fn new(content: NodeContent, owner: u16) -> Self {
        Self {
            content,
            style: Style::default(),
            parent: NONE,
            first_child: NONE,
            next: NONE,
            key: None,
            id: None,
            role: Role::None,
            owner,
            tap: None,
            drag: None,
            translate: (0, 0),
            opacity: None,
            rect: Rect::default(),
            screen: Rect::default(),
            paint_dirty: true,
            state: State::empty(),
            flow_size: 0,
            flow_resized: false,
            scrolled: false,
            measured: None,
        }
    }
}
