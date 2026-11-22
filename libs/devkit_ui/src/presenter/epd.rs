/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::sync::atomic::AtomicUsize;
use core::time::Duration;

use bsp_rs::display::{self, Epd, EpdMode, FrameBuffer, Gram, RefreshUnit, Rotation, Waveform};
use devkit_gfx::{Damage, DisplayList, GlyphCache, Rect};
use idf::heap::Caps;
use idf::log::error;
use idf::{Result, thread};

use super::writer::StripWriter;
use super::{Config, Grid, Presenter, Stats, Strips};
use crate::blit::NewBlitter;
use crate::epd::EpdAction;
use crate::external::{Buffers, NO_BACK};

const TAG: &CStr = c"presenter";

const MAX_REGIONS: usize = 8;
const BUSY_POLL: Duration = Duration::from_millis(10);

/// Puts display lists on a panel driving electrophoretic glass: what changed
/// is written into the panel's memory, then driven as each change asks.
pub(crate) struct EpdPresenter {
    writer: StripWriter,
    epd: Epd<'static>,
    unit: RefreshUnit,
    /// Everything to draw.
    damage: Damage<MAX_REGIONS>,
    /// The changes, on the grid, with what each asks.
    asked: Vec<(Rect, EpdAction), Caps>,
    hole: Option<Rect>,
    over: Option<Rect>,
    fresh: bool,
}

impl EpdPresenter {
    pub(crate) fn new(gram: Gram<'static>, epd: Epd<'static>, config: Config) -> Result<Self> {
        let caps = config.data_caps;
        epd.set_mode(EpdMode::Hold)?;
        let writer = StripWriter::new(gram, config)?;
        let unit = epd.refresh_unit();
        let mut presenter =
            Self { writer, epd, unit, damage: Damage::new(Rect::default()), asked: Vec::new_in(caps), hole: None, over: None, fresh: true };
        presenter.reset();
        Ok(presenter)
    }

    /// What to drive the new picture with comes from `invalidate_all`.
    fn reset(&mut self) {
        self.damage = Damage::new(self.writer.area());
        self.damage.add_full();
        self.asked.clear();
        self.fresh = true;
    }

    /// Drives `rect` of the logical picture as `action` says, after what was
    /// drawn there has been written.
    fn drive(&self, rect: Rect, action: EpdAction) {
        let Some(panel) = self.writer.panel_rect(rect) else { return };
        let result = match action {
            EpdAction::Refresh(w) | EpdAction::CleanThen(w) => self.epd.refresh(panel, w, false),
            EpdAction::RefreshAll(w) => self.epd.refresh(panel, w, true),
        };
        if let Err(err) = result {
            error!(TAG, "refresh {panel:?}: {err}");
        }
    }

    /// What of `rect` this presenter drives: not what a surface shows, unless
    /// the UI is written over it.
    fn ours(&self, rect: Rect) -> impl Iterator<Item = Rect> + use<> {
        let (hole, over) = (self.hole.unwrap_or_default(), self.over.unwrap_or_default());
        rect.subtract(&hole).chain([rect.intersect(&over)].into_iter().filter(|r| !r.is_empty()))
    }

    /// Drives what was asked, the actions that do more first; an area is
    /// driven once, by the first that reaches it.
    fn drive_asked(&mut self) {
        let mut asked = core::mem::replace(&mut self.asked, Vec::new_in(Caps::INTERNAL));
        asked.sort_by_key(|(_, action)| core::cmp::Reverse(action.strength()));
        if self.unit != RefreshUnit::Pixel {
            let Some(&(_, strongest)) = asked.first() else {
                self.asked = asked;
                return;
            };
            let all = asked.iter().fold(Rect::default(), |all, (rect, _)| all.union(rect));
            for piece in self.ours(all) {
                self.drive(piece, strongest);
            }
        } else {
            let mut done: Vec<Rect> = Vec::new();
            for &(rect, action) in &asked {
                let mut pieces: Vec<Rect> = self.ours(rect).collect();
                for taken in &done {
                    pieces = pieces.iter().flat_map(|p| p.subtract(taken)).collect();
                }
                for piece in pieces {
                    self.drive(piece, action);
                }
                done.push(rect);
            }
        }
        asked.clear();
        self.asked = asked;
    }

    /// Drives `Fast` as it is written, when the panel drives pixels on their
    /// own and it is all that is asked; other waveforms change all at once,
    /// after everything is written. Not while a surface shows: its writer
    /// shares the mode.
    fn write_mode(&self) -> Option<EpdMode> {
        if self.unit != RefreshUnit::Pixel || self.hole.is_some() {
            return None;
        }
        let &(_, action) = self.asked.first()?;
        if self.asked.iter().any(|&(_, a)| a != action) {
            return None;
        }
        match action {
            EpdAction::Refresh(Waveform::Fast) => Some(EpdMode::Refresh(Waveform::Fast)),
            EpdAction::RefreshAll(Waveform::Fast) if self.damage.regions().iter().all(|r| self.asked.iter().any(|(rect, _)| rect.contains(r))) => {
                Some(EpdMode::RefreshAll(Waveform::Fast))
            }
            _ => None,
        }
    }

    fn set_mode(&self, mode: EpdMode) {
        if let Err(err) = self.epd.set_mode(mode) {
            error!(TAG, "mode {mode:?}: {err}");
        }
    }

    /// Waits while a write to what is to be drawn would wait for the glass.
    fn wait_glass(&self) {
        loop {
            let busy = self.damage.regions().iter().any(|r| {
                self.writer.panel_rect(*r).is_some_and(|panel| self.epd.busy(panel).inspect_err(|err| error!(TAG, "busy: {err}")).unwrap_or(false))
            });
            if !busy {
                return;
            }
            thread::sleep(BUSY_POLL);
        }
    }
}

impl Presenter for EpdPresenter {
    fn area(&self) -> Rect {
        self.writer.area()
    }

    fn grid(&self) -> Grid {
        self.writer.grid()
    }

    fn invalidate(&mut self, rect: Rect, action: EpdAction) {
        let rect = self.writer.grid().around(rect).intersect(&self.writer.area());
        if rect.is_empty() {
            return;
        }
        self.damage.add(rect);
        self.asked.push((rect, action));
    }

    fn invalidate_all(&mut self, action: EpdAction) {
        self.damage.add_full();
        self.asked.push((self.writer.area(), action));
    }

    fn redrive(&mut self, rect: Rect, action: EpdAction) {
        if let EpdAction::CleanThen(_) = action {
            self.invalidate(rect, action);
            return;
        }
        let rect = self.writer.grid().around(rect).intersect(&self.writer.area());
        for piece in self.ours(rect) {
            self.drive(piece, action);
        }
    }

    fn has_new_damage(&self) -> bool {
        self.fresh
    }

    fn acquire(&mut self) -> Result<()> {
        Ok(())
    }

    fn regions(&mut self) -> Result<&[Rect]> {
        self.wait_glass();
        Ok(self.damage.regions())
    }

    fn render(&mut self, list: &DisplayList<'_, Caps>, cache: &mut GlyphCache<Caps>) -> Result<Stats> {
        self.fresh = false;
        if self.damage.is_empty() {
            self.asked.clear();
            return Ok(Stats::default());
        }
        for &(rect, action) in &self.asked {
            if let EpdAction::CleanThen(_) = action {
                for piece in self.ours(rect) {
                    let Some(panel) = self.writer.panel_rect(piece) else { continue };
                    if let Err(err) = self.epd.clear(panel) {
                        error!(TAG, "clear {panel:?}: {err}");
                    }
                }
            }
        }
        let (hole, over) = (self.hole.unwrap_or_default(), self.over.unwrap_or_default());
        let mode = self.write_mode();
        if let Some(mode) = mode {
            self.set_mode(mode);
        }
        let stats = self.writer.render(list, cache, self.damage.regions(), hole, over);
        if mode.is_some() {
            self.set_mode(EpdMode::Hold);
            self.asked.clear();
        }
        let stats = stats?;
        self.damage.clear();
        self.drive_asked();
        Ok(stats)
    }

    fn set_rotation(&mut self, rotation: Rotation) -> Result<()> {
        let result = self.writer.set_rotation(rotation);
        self.reset();
        result
    }

    fn set_strips(&mut self, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        self.writer.set_strips(strips, lanes)
    }

    fn changes_display(&self, config: &display::Config) -> bool {
        config.pixel_format.is_some_and(|f| f != self.writer.display().pixel_format()) || config.fb_num != 0
    }

    fn fits_display(&self, config: &display::Config, strips: &Strips, lanes: usize) -> bool {
        let format = config.pixel_format.unwrap_or_else(|| self.writer.display().pixel_format());
        self.writer.fits(format, strips, lanes)
    }

    unsafe fn set_display(&mut self, config: &display::Config, strips: Strips, lanes: usize) -> core::result::Result<Strips, Strips> {
        if self.changes_display(config) {
            error!(TAG, "the panel keeps its format and has no framebuffers");
            return Err(strips);
        }
        self.set_strips(strips, lanes)
    }

    fn set_external(&mut self, hole: Option<Rect>, _hold: bool, over: Option<Rect>) -> bool {
        (self.hole, self.over) = (hole, over);
        false
    }

    fn behind(&mut self) -> bool {
        false
    }

    fn buffers(&self) -> Buffers {
        Buffers { latest: Arc::new(AtomicUsize::new(0)), back: Arc::new(AtomicUsize::new(NO_BACK)), count: 0 }
    }

    fn shown(&self) -> Option<FrameBuffer> {
        None
    }

    fn blitter(&self) -> Option<NewBlitter> {
        None
    }

    fn half_pictures(&self) -> bool {
        false
    }
}
