/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use core::ptr::NonNull;
use core::slice;
use core::time::Duration;

use devkit_gfx::{Image, ImageFormat, Size, Target};
use idf::{Error, Result};
use media_engine::{Job, Slot, Srm};

struct Lane {
    slot: Slot,
    target: Option<Target<'static>>,
}

/// Copies with the 2D engine, up to a number in flight, finished in the order
/// they went. The pixels of each stay in use until it is waited for.
pub(crate) struct Blits {
    jobs: VecDeque<Job<'static>>,
    lanes: Box<[Lane]>,
    next: usize,
}

unsafe impl Send for Blits {}

impl Blits {
    pub(crate) fn new(max_pending: usize) -> Result<Self> {
        media_engine::ppa()?;
        let lanes: Box<[Lane]> = (0..max_pending.max(1)).map(|_| Lane { slot: Slot::new(), target: None }).collect();
        Ok(Self { jobs: VecDeque::with_capacity(lanes.len()), lanes, next: 0 })
    }

    /// Queues `op` into `target`; both must stay valid until it is waited for.
    pub(crate) fn run(&mut self, target: Target<'static>, op: &Srm<'static>) -> Result<()> {
        if self.jobs.len() == self.lanes.len() {
            return Err(Error::INVALID_STATE);
        }
        // Jobs go before their lanes, and a lane is handed out again only once
        // the job that used it has gone.
        let lane: &'static mut Lane = unsafe { &mut *(&raw mut self.lanes[self.next]) };
        self.next = (self.next + 1) % self.lanes.len();
        let target = lane.target.insert(target);
        let job = media_engine::ppa()?.srm(op, target, &mut lane.slot)?;
        self.jobs.push_back(job);
        Ok(())
    }

    pub(crate) fn pending(&self) -> usize {
        self.jobs.len()
    }

    /// Waits for the oldest copy; `false` on timeout, with it still pending.
    pub(crate) fn wait_timeout(&mut self, timeout: Duration) -> bool {
        let Some(job) = self.jobs.front_mut() else { return true };
        match job.wait_timeout(timeout) {
            None => false,
            Some(result) => {
                if let Err(err) = result {
                    idf::log::error!(c"ui", "copy: {err}");
                }
                self.jobs.pop_front();
                true
            }
        }
    }
}

/// `size` of `format` packed at `ptr`, for as long as the caller keeps it.
pub(crate) unsafe fn image(ptr: *const u8, format: ImageFormat, size: Size) -> Image<'static> {
    let len = format.row_bytes(usize::from(size.width)) * usize::from(size.height);
    Image::from_pixels(format, size.width, size.height, usize::from(size.width), unsafe { slice::from_raw_parts(ptr, len) })
}

/// The same, written into; nothing else may use the pixels meanwhile.
pub(crate) unsafe fn target(ptr: *mut u8, format: ImageFormat, size: Size) -> Result<Target<'static>> {
    let len = format.row_bytes(usize::from(size.width)) * usize::from(size.height);
    let ptr = NonNull::new(ptr).ok_or(Error::INVALID_ARG)?;
    unsafe { Target::from_raw(ptr, len, format, size, usize::from(size.width)) }.ok_or(Error::INVALID_ARG)
}
