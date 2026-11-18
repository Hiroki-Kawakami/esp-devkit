/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use core::time::Duration;

use devkit_gfx::Target;
use idf::{Error, Result};
use media_engine::{Job, Slot, Srm};

use crate::blit::{Blend, Blit, Blitter};

struct Lane {
    slot: Slot,
    target: Option<Target<'static>>,
}

/// `Blitter` on the PPA.
pub(crate) struct PpaBlitter {
    jobs: VecDeque<Job<'static>>,
    lanes: Box<[Lane]>,
    next: usize,
}

unsafe impl Send for PpaBlitter {}

impl PpaBlitter {
    pub(crate) fn new(max_pending: usize) -> Result<Self> {
        media_engine::ppa()?;
        let lanes: Box<[Lane]> = (0..max_pending.max(1)).map(|_| Lane { slot: Slot::new(), target: None }).collect();
        Ok(Self { jobs: VecDeque::with_capacity(lanes.len()), lanes, next: 0 })
    }

    pub(crate) fn boxed(max_pending: usize) -> Result<Box<dyn Blitter>> {
        Ok(Box::new(Self::new(max_pending)?))
    }

    fn lane(&mut self, target: Target<'static>) -> Result<(&'static mut Slot, &'static mut Target<'static>)> {
        if self.jobs.len() == self.lanes.len() {
            return Err(Error::INVALID_STATE);
        }
        // Jobs go before their lanes, and a lane is handed out again only once
        // the job that used it has gone.
        let lane: &'static mut Lane = unsafe { &mut *(&raw mut self.lanes[self.next]) };
        self.next = (self.next + 1) % self.lanes.len();
        Ok((&mut lane.slot, lane.target.insert(target)))
    }
}

impl Blitter for PpaBlitter {
    fn blit(&mut self, target: Target<'static>, op: &Blit<'static>) -> Result<()> {
        let mut srm = Srm::new(op.src);
        (srm.src_rect, srm.at, srm.rotation) = (op.src_rect, op.at, op.rotation);
        (srm.scale_x, srm.scale_y) = (op.scale, op.scale);
        let (slot, target) = self.lane(target)?;
        let job = media_engine::ppa()?.srm(&srm, target, slot)?;
        self.jobs.push_back(job);
        Ok(())
    }

    fn blend(&mut self, target: Target<'static>, op: &Blend<'static>) -> Result<()> {
        let mut blend = media_engine::Blend::new(op.fg, op.at);
        (blend.fg_at, blend.size) = ((op.fg_rect.x, op.fg_rect.y), (op.fg_rect.w, op.fg_rect.h));
        let (slot, target) = self.lane(target)?;
        let job = media_engine::ppa()?.blend(&blend, target, slot)?;
        self.jobs.push_back(job);
        Ok(())
    }

    fn pending(&self) -> usize {
        self.jobs.len()
    }

    fn wait_timeout(&mut self, timeout: Duration) -> Option<Result<()>> {
        let Some(job) = self.jobs.front_mut() else { return Some(Ok(())) };
        let result = job.wait_timeout(timeout)?;
        self.jobs.pop_front();
        Some(result)
    }
}
