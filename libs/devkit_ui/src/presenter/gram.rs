/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::ffi::CStr;
use core::sync::atomic::AtomicUsize;

use bsp_rs::display::{self, FrameBuffer, Gram, Rotation};
use devkit_gfx::{Damage, DisplayList, GlyphCache, Rect};
use idf::heap::Caps;
use idf::log::error;
use idf::Result;

use super::writer::StripWriter;
use super::{Config, Grid, Presenter, Stats, Strips};
use crate::blit::NewBlitter;
use crate::epd::EpdAction;
use crate::external::{Buffers, NO_BACK};

const TAG: &CStr = c"presenter";

const MAX_REGIONS: usize = 8;

/// Puts display lists on a panel that keeps the picture in its own memory:
/// damaged rows are drawn into strips and written while the next ones are
/// drawn.
pub(crate) struct GramPresenter {
    writer: StripWriter,
    damage: Damage<MAX_REGIONS>,
    hole: Option<Rect>,
    over: Option<Rect>,
    fresh: bool,
}

impl GramPresenter {
    pub(crate) fn new(gram: Gram<'static>, config: Config) -> Result<Self> {
        let writer = StripWriter::new(gram, config)?;
        let mut presenter = Self { writer, damage: Damage::new(Rect::default()), hole: None, over: None, fresh: true };
        presenter.reset();
        Ok(presenter)
    }

    fn reset(&mut self) {
        self.damage = Damage::new(self.writer.area());
        self.damage.add_full();
        self.fresh = true;
    }
}

impl Presenter for GramPresenter {
    fn area(&self) -> Rect {
        self.writer.area()
    }

    fn grid(&self) -> Grid {
        self.writer.grid()
    }

    fn invalidate(&mut self, rect: Rect, _action: EpdAction) {
        self.damage.add(self.writer.grid().around(rect));
    }

    fn invalidate_all(&mut self, _action: EpdAction) {
        self.damage.add_full();
    }

    fn redrive(&mut self, _rect: Rect, _action: EpdAction) {}

    fn has_new_damage(&self) -> bool {
        self.fresh
    }

    fn acquire(&mut self) -> Result<()> {
        Ok(())
    }

    fn regions(&mut self) -> Result<&[Rect]> {
        Ok(self.damage.regions())
    }

    fn render(&mut self, list: &DisplayList<'_, Caps>, cache: &mut GlyphCache<Caps>) -> Result<Stats> {
        self.fresh = false;
        if self.damage.is_empty() {
            return Ok(Stats::default());
        }
        let (hole, over) = (self.hole.unwrap_or_default(), self.over.unwrap_or_default());
        let stats = self.writer.render(list, cache, self.damage.regions(), hole, over)?;
        self.damage.clear();
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
