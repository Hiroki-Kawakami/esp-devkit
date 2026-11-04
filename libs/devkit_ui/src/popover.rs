/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_gfx::Rect;

use crate::app::Handle;
use crate::component::{Component, Cx};
use crate::element::{Element, Ui};

/// Which side of its anchor a popover opens on. `Auto` takes below where the
/// popover fits, then above, then the roomier side.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Placement {
    #[default]
    Auto,
    Below,
    Above,
}

/// The anchor's edge a popover lines up with. Where that runs past the
/// screen, it lines up with the other edge instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PopoverAlign {
    #[default]
    Start,
    End,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Anchored {
    pub(crate) anchor: Rect,
    pub(crate) bounds: Rect,
    pub(crate) placement: Placement,
    pub(crate) align: PopoverAlign,
}

impl Anchored {
    pub(crate) fn place(&self, mut measure: impl FnMut(i16) -> (i16, i16)) -> Rect {
        let (anchor, bounds) = (self.anchor, self.bounds);
        let below = bounds.bottom() - anchor.bottom();
        let above = anchor.y - bounds.y;
        let (_, wanted) = measure(bounds.h);
        let downward = match self.placement {
            Placement::Below => true,
            Placement::Above => false,
            Placement::Auto => wanted <= below || (wanted > above && below >= above),
        };
        let room = if downward { below } else { above }.max(0);
        let (w, h) = measure(room);
        let h = h.min(room);
        let x = match self.align {
            PopoverAlign::Start if anchor.x + w > bounds.right() => anchor.right() - w,
            PopoverAlign::Start => anchor.x,
            PopoverAlign::End if anchor.right() - w < bounds.x => anchor.x,
            PopoverAlign::End => anchor.right() - w,
        };
        let x = x.min(bounds.right() - w).max(bounds.x);
        let y = if downward { anchor.bottom() } else { anchor.y - h };
        Rect::new(x, y, w, h)
    }
}

type PopoverFn<P, M> = for<'a> fn(&'a P, &'a Ui<M>) -> Element<'a, M>;

/// A popup by `anchor` showing what `view` makes of `props`, sized by it. What
/// is inside sends to `target`, which closes the popover, as a tap outside
/// does. Where the chosen side is too short, the popover gets that height.
pub struct Popover<P: 'static, M: Copy + 'static> {
    anchor: Rect,
    props: P,
    view: PopoverFn<P, M>,
    target: Handle<M>,
    placement: Placement,
    align: PopoverAlign,
}

impl<P: 'static, M: Copy + 'static> Popover<P, M> {
    pub fn new(anchor: Rect, props: P, view: PopoverFn<P, M>, target: Handle<M>) -> Self {
        Self { anchor, props, view, target, placement: Placement::Auto, align: PopoverAlign::Start }
    }

    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    pub fn align(mut self, align: PopoverAlign) -> Self {
        self.align = align;
        self
    }
}

impl<P: 'static, M: Copy + 'static> Component for Popover<P, M> {
    type Msg = M;

    fn update(&mut self, msg: M, cx: &mut Cx<Self>) {
        cx.send_to(&self.target, msg);
        cx.close_popup();
    }

    fn view<'a>(&'a self, ui: &'a Ui<M>) -> Element<'a, M> {
        popover(ui, self.anchor, self.placement, self.align, (self.view)(&self.props, ui))
    }
}

/// `panel` placed by `anchor` in what the insets leave of the screen.
pub(crate) fn popover<'a, M: Copy>(ui: &'a Ui<M>, anchor: Rect, placement: Placement, align: PopoverAlign, panel: Element<'a, M>) -> Element<'a, M> {
    let ((width, height), insets) = (ui.screen_size(), ui.insets());
    let bounds = Rect::new(insets.left, insets.top, width - insets.left - insets.right, height - insets.top - insets.bottom);
    ui.column([panel.anchored(Anchored { anchor, bounds, placement, align })])
}
