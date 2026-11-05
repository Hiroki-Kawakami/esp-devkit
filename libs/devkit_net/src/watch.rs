/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::{Poll, Waker};

use idf::sync::Mutex;

struct Slot<T> {
    value: T,
    version: u32,
    waiters: Vec<Waker>,
}

/// A value that tasks wait to change; any number of waiters.
pub(crate) struct Watch<T> {
    slot: Mutex<Slot<T>>,
}

impl<T: Clone> Watch<T> {
    pub const fn new(value: T) -> Self {
        Self { slot: Mutex::new(Slot { value, version: 0, waiters: Vec::new() }) }
    }

    pub fn set(&self, value: T) -> u32 {
        self.update(|_| Some(value))
    }

    pub fn update(&self, f: impl FnOnce(&T) -> Option<T>) -> u32 {
        let mut s = self.slot.lock();
        let Some(value) = f(&s.value) else { return s.version };
        s.value = value;
        s.version = s.version.wrapping_add(1);
        for w in s.waiters.drain(..) {
            w.wake();
        }
        s.version
    }

    pub fn get(&self) -> (T, u32) {
        let s = self.slot.lock();
        (s.value.clone(), s.version)
    }

    /// The value once its version differs from `seen`.
    pub async fn after(&self, seen: u32) -> (T, u32) {
        poll_fn(|cx| {
            let mut s = self.slot.lock();
            if s.version != seen {
                return Poll::Ready((s.value.clone(), s.version));
            }
            if !s.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                s.waiters.push(cx.waker().clone());
            }
            Poll::Pending
        })
        .await
    }
}
