/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::future::Future;
use core::marker::PhantomPinned;
use core::pin::Pin;
use core::ptr;
use core::task::{Context, Poll, Waker};

use super::{Inner, current};

/// Ticks wrap; deadlines are compared within half the range.
pub(super) fn reached(now: u32, deadline: u32) -> bool {
    deadline.wrapping_sub(now) as i32 <= 0
}

/// Linked into its executor's list while it waits; the links and the waker
/// belong to the executor's critical section.
pub(super) struct Node {
    pub deadline: u32,
    pub waker: Option<Waker>,
    pub prev: *mut Node,
    pub next: *mut Node,
}

enum Deadline {
    After(u32),
    At(u32),
}

/// Resolves on the first poll at or after its deadline tick, on a devkit
/// executor.
pub struct Timer {
    deadline: Deadline,
    executor: Option<Arc<Inner>>,
    node: UnsafeCell<Node>,
    _pinned: PhantomPinned,
}

unsafe impl Send for Timer {}

/// `ticks` from the first poll.
pub fn sleep(ticks: u32) -> Timer {
    Timer::new(Deadline::After(ticks))
}

pub fn sleep_until(tick: u32) -> Timer {
    Timer::new(Deadline::At(tick))
}

impl Timer {
    fn new(deadline: Deadline) -> Self {
        Self {
            deadline,
            executor: None,
            node: UnsafeCell::new(Node { deadline: 0, waker: None, prev: ptr::null_mut(), next: ptr::null_mut() }),
            _pinned: PhantomPinned,
        }
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = unsafe { self.get_unchecked_mut() };
        let executor = this.executor.get_or_insert_with(|| current(cx).expect("not on a devkit executor"));
        let now = executor.now();
        if let Deadline::After(ticks) = this.deadline {
            this.deadline = Deadline::At(now.wrapping_add(ticks));
        }
        let Deadline::At(deadline) = this.deadline else { unreachable!() };
        if reached(now, deadline) {
            let old = executor.unlink_timer(this.node.get());
            drop(old);
            return Poll::Ready(());
        }
        let waker = cx.waker().clone();
        let old = executor.link_timer(this.node.get(), deadline, waker);
        drop(old);
        Poll::Pending
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(executor) = &self.executor {
            let old = executor.unlink_timer(self.node.get());
            drop(old);
        }
    }
}

/// `timeout` ran out before the future finished.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Elapsed;

/// `future`'s output, unless `ticks` pass first; the future is dropped then.
pub fn timeout<F: Future>(ticks: u32, future: F) -> Timeout<F> {
    Timeout { future, timer: sleep(ticks) }
}

// Not an async fn: its state would hold `future` twice, as the argument and
// as the pinned local, and every wrapper around it once more.
pub struct Timeout<F> {
    future: F,
    timer: Timer,
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, Elapsed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        if let Poll::Ready(output) = unsafe { Pin::new_unchecked(&mut this.future) }.poll(cx) {
            return Poll::Ready(Ok(output));
        }
        unsafe { Pin::new_unchecked(&mut this.timer) }.poll(cx).map(|()| Err(Elapsed))
    }
}

impl Inner {
    fn now(&self) -> u32 {
        let mut clock = None;
        self.critical(|| clock = unsafe { self.shared() }.clock.clone());
        clock.expect("executor has no runner").now()
    }

    /// The waker it replaced, to drop outside the critical section.
    fn link_timer(&self, node: *mut Node, deadline: u32, waker: Waker) -> Option<Waker> {
        self.critical(|| unsafe {
            let shared = self.shared();
            let old = (*node).waker.replace(waker);
            (*node).deadline = deadline;
            if old.is_none() {
                (*node).prev = ptr::null_mut();
                (*node).next = shared.timers;
                if !shared.timers.is_null() {
                    (*shared.timers).prev = node;
                }
                shared.timers = node;
            }
            old
        })
    }

    fn unlink_timer(&self, node: *mut Node) -> Option<Waker> {
        self.critical(|| unsafe { self.unlink_locked(node) })
    }

    /// Inside the critical section.
    unsafe fn unlink_locked(&self, node: *mut Node) -> Option<Waker> {
        let shared = unsafe { self.shared() };
        let waker = unsafe { (*node).waker.take()? };
        let (prev, next) = unsafe { ((*node).prev, (*node).next) };
        if prev.is_null() {
            shared.timers = next;
        } else {
            unsafe { (*prev).next = next };
        }
        if !next.is_null() {
            unsafe { (*next).prev = prev };
        }
        Some(waker)
    }

    /// Wakes every timer due at `now`; the ticks to the next one, if any.
    pub(super) fn fire_timers(&self, now: u32) -> Option<u32> {
        loop {
            let mut due = None;
            let mut next: Option<u32> = None;
            self.critical(|| unsafe {
                let mut node = self.shared().timers;
                while !node.is_null() {
                    let deadline = (*node).deadline;
                    if reached(now, deadline) {
                        due = self.unlink_locked(node);
                        return;
                    }
                    let left = deadline.wrapping_sub(now);
                    next = Some(next.map_or(left, |n| n.min(left)));
                    node = (*node).next;
                }
            });
            match due {
                Some(waker) => waker.wake(),
                None => return next,
            }
        }
    }
}
