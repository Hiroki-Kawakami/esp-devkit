/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use idf::heap::Caps;
use idf::sync::{Mutex, MutexGuard};

use crate::{Error, Result};

/// Setters write pending parameters under the lock; process adopts them at a
/// block boundary only when the lock is free, so it never waits on a setter.
pub(crate) struct Shared<T> {
    slot: Mutex<T>,
    dirty: AtomicBool,
}

impl<T: Send> Shared<T> {
    pub(crate) fn new(value: T, caps: Caps) -> Result<Arc<Self, Caps>> {
        let shared = Self { slot: Mutex::try_new(value)?, dirty: AtomicBool::new(false) };
        Arc::try_new_in(shared, caps).map_err(|_| Error::NoMemory)
    }

    pub(crate) fn publish<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut slot = self.slot.lock();
        let result = f(&mut slot);
        self.dirty.store(true, Ordering::SeqCst);
        result
    }

    pub(crate) fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.slot.lock())
    }

    #[inline]
    pub(crate) fn try_adopt(&self) -> Option<MutexGuard<'_, T>> {
        if !self.dirty.load(Ordering::SeqCst) {
            return None;
        }
        let slot = self.slot.try_lock()?;
        self.dirty.store(false, Ordering::SeqCst);
        Some(slot)
    }
}
