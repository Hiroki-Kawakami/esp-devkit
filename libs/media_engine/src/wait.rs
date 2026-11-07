/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use core::time::Duration;

use devkit::executor::Park;
use idf::executor::TaskPark;
use idf::time::Instant;

/// Work an interrupt finishes, polled with a waker it may keep until told to
/// forget it.
pub(crate) trait Pollable {
    type Output;

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Self::Output>;
    fn forget_waker(&mut self);
}

/// Parks the calling task until `work` is ready; `None` once `timeout` has
/// passed. block_on would set up a whole executor for each wait.
pub(crate) fn block<P: Pollable>(work: &mut P, timeout: Option<Duration>) -> Option<P::Output> {
    let park = TaskPark::current();
    let waker = unsafe { Waker::from_raw(park_waker(&park)) };
    let mut cx = Context::from_waker(&waker);
    let start = Instant::now();
    let tick = idf::executor::tick_duration(1).as_micros().max(1);
    loop {
        if let Poll::Ready(out) = work.poll(&mut cx) {
            work.forget_waker();
            return Some(out);
        }
        let ticks = match timeout {
            None => None,
            Some(timeout) => {
                let left = timeout.saturating_sub(start.elapsed());
                if left.is_zero() {
                    work.forget_waker();
                    return None;
                }
                Some((left.as_micros() / tick) as u32 + 1)
            }
        };
        park.park(ticks);
    }
}

/// Wakes the task that owns `park`; valid while `park` is, which `block`
/// keeps by having the waker forgotten before it returns.
fn park_waker(park: &TaskPark) -> RawWaker {
    unsafe fn clone(data: *const ()) -> RawWaker {
        RawWaker::new(data, &VTABLE)
    }
    unsafe fn wake(data: *const ()) {
        unsafe { (*data.cast::<TaskPark>()).unpark() };
    }
    unsafe fn drop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake, drop);
    RawWaker::new(core::ptr::from_ref(park).cast(), &VTABLE)
}
