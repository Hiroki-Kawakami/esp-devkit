/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! The waveforms of glass driven line by line from the chip, replaceable
//! while it runs.

use alloc::boxed::Box;

use idf::{Error, Result};

use super::Display;
use crate::driver::epd_ll::waveform;

/// One row per frame: 16 words by the gray on the glass, each holding 2 bits
/// per target gray, `to * 2` up (0 hold, 1 toward black, 2 toward white). A
/// row of `0xFFFF_FFFF` words drives nothing and may be cut short while a
/// draw waits; only a tail of them may be.
pub type Lut = Box<[[u32; 16]]>;

pub const STEP_MAX: usize = waveform::STEP_MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Fast,
    Quality,
    Text,
    /// What `Epd::clear` replays.
    Clear,
}

impl Slot {
    fn index(self) -> usize {
        match self {
            Self::Fast => 0,
            Self::Quality => 1,
            Self::Text => 2,
            Self::Clear => 3,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Waveforms<'a>(&'a Display);

impl<'a> Waveforms<'a> {
    pub fn of(display: &'a Display) -> Self {
        Self(display)
    }

    /// Waits until nothing is being driven, then replays `lut` for `slot`
    /// from the next refresh on. Returns the table it replaces when that one
    /// was set here too. `INVALID_ARG` for an empty or longer than `STEP_MAX`
    /// table, or a row mixing `0xFFFF_FFFF` words with others.
    pub fn set(self, slot: Slot, lut: Lut) -> Result<Option<Lut>> {
        self.0.driver().set_waveform(slot.index(), lut)
    }

    /// A copy of what `slot` replays.
    pub fn get(self, slot: Slot) -> Result<Lut> {
        self.0.driver().waveform(slot.index()).ok_or(Error::NOT_FOUND)
    }
}
