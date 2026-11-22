/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! What a panel that drives electrophoretic glass does with each change of
//! the picture. Other panels ignore all of it.

use alloc::vec::Vec;

use bsp_rs::display::Waveform;

use crate::id::Id;

/// What changed a part of the picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EpdCause {
    /// Pressed, focused, selected and the other states, and the caret.
    State,
    Scroll,
    /// What an element shows, and elements coming and going.
    Content,
    /// Elements moving with nothing else changed.
    Layout,
    /// Screens and popups coming and going, and the picture turning.
    Screen,
    /// What an image shows.
    Image,
}

const CAUSES: [EpdCause; 6] = [EpdCause::State, EpdCause::Scroll, EpdCause::Content, EpdCause::Layout, EpdCause::Screen, EpdCause::Image];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EpdAction {
    /// Drives the pixels that changed.
    Refresh(Waveform),
    /// Drives every pixel of the area, which clears ghosting.
    RefreshAll(Waveform),
    /// Drives the area to white, then draws it.
    CleanThen(Waveform),
}

impl EpdAction {
    /// Higher does more for the glass.
    pub(crate) fn strength(self) -> u8 {
        let (kind, waveform) = match self {
            Self::Refresh(w) => (0, w),
            Self::RefreshAll(w) => (1, w),
            Self::CleanThen(w) => (2, w),
        };
        let waveform = match waveform {
            Waveform::Fast => 0,
            Waveform::Text => 1,
            Waveform::Quality => 2,
        };
        kind * 3 + waveform
    }

    /// The one that does more for the glass.
    pub(crate) fn max(self, other: Self) -> Self {
        if other.strength() > self.strength() { other } else { self }
    }
}

/// The action for each cause; where causes meet, the action that does more.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EpdPolicy {
    pub state: EpdAction,
    pub scroll: EpdAction,
    pub content: EpdAction,
    pub layout: EpdAction,
    pub screen: EpdAction,
    pub image: EpdAction,
}

impl EpdPolicy {
    pub const DEFAULT: Self = Self {
        state: EpdAction::Refresh(Waveform::Fast),
        scroll: EpdAction::Refresh(Waveform::Fast),
        content: EpdAction::Refresh(Waveform::Text),
        layout: EpdAction::Refresh(Waveform::Text),
        screen: EpdAction::RefreshAll(Waveform::Text),
        image: EpdAction::Refresh(Waveform::Quality),
    };

    pub fn action(&self, cause: EpdCause) -> EpdAction {
        match cause {
            EpdCause::State => self.state,
            EpdCause::Scroll => self.scroll,
            EpdCause::Content => self.content,
            EpdCause::Layout => self.layout,
            EpdCause::Screen => self.screen,
            EpdCause::Image => self.image,
        }
    }

    pub(crate) fn resolve(&self, causes: Causes) -> Option<EpdAction> {
        CAUSES.iter().filter(|c| causes.has(**c)).map(|c| self.action(*c)).reduce(EpdAction::max)
    }
}

impl Default for EpdPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A set of causes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Causes(u8);

impl Causes {
    pub(crate) const fn of(cause: EpdCause) -> Self {
        Self(1 << cause as u8)
    }

    pub(crate) const fn with(self, cause: EpdCause) -> Self {
        Self(self.0 | Self::of(cause).0)
    }

    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) const fn has(self, cause: EpdCause) -> bool {
        self.0 & Self::of(cause).0 != 0
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// What `EpdRequests::refresh` drives again.
#[derive(Clone, Copy, Debug)]
pub enum EpdScope {
    Screen,
    /// The element as it shows on the screen.
    Element(Id),
}

/// What a component asks of the glass, as `Cx::epd`.
#[derive(Default)]
pub struct EpdRequests {
    pub(crate) next: Option<EpdAction>,
    pub(crate) refresh: Vec<(EpdScope, EpdAction)>,
}

impl EpdRequests {
    /// Does `action` with everything the next frame changes, whatever changed
    /// it and whatever the elements ask.
    pub fn next(&mut self, action: EpdAction) {
        self.next = Some(self.next.map_or(action, |n| n.max(action)));
    }

    /// Does `action` with `scope` as it shows, changed or not. `Refresh` finds
    /// no changed pixel there; `RefreshAll` and `CleanThen` are what it is for.
    pub fn refresh(&mut self, scope: EpdScope, action: EpdAction) {
        self.refresh.push((scope, action));
    }

    pub(crate) fn merge(&mut self, other: Self) {
        if let Some(action) = other.next {
            self.next(action);
        }
        self.refresh.extend(other.refresh);
    }
}
