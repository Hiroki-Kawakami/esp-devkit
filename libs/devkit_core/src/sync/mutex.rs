/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::cell::UnsafeCell;
use core::future::Future;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::task::{Context, Poll};

use super::Platform;
use super::raw::{LockWait, RawLock};

pub struct Mutex<T: ?Sized, P: Platform> {
    pub(super) raw: RawLock<P>,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send, P: Platform> Send for Mutex<T, P> {}
unsafe impl<T: ?Sized + Send, P: Platform> Sync for Mutex<T, P> {}

impl<T, P: Platform> Mutex<T, P> {
    pub const fn new(value: T) -> Self {
        Self { raw: RawLock::new(), data: UnsafeCell::new(value) }
    }

    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized, P: Platform> Mutex<T, P> {
    /// Blocks the calling task, and so every future of an executor it runs.
    pub fn lock(&self) -> MutexGuard<'_, T, P> {
        self.raw.write();
        MutexGuard { mutex: self, _data: PhantomData }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T, P>> {
        self.raw.try_write().then(|| MutexGuard { mutex: self, _data: PhantomData })
    }

    pub fn lock_async(&self) -> MutexLock<'_, T, P> {
        MutexLock { mutex: self, wait: LockWait::new(&self.raw, false) }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<T: Default, P: Platform> Default for Mutex<T, P> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// May be dropped on another task than the one that locked.
pub struct MutexGuard<'a, T: ?Sized, P: Platform> {
    mutex: &'a Mutex<T, P>,
    _data: PhantomData<&'a mut T>,
}

impl<T: ?Sized, P: Platform> Deref for MutexGuard<'_, T, P> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized, P: Platform> DerefMut for MutexGuard<'_, T, P> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized, P: Platform> Drop for MutexGuard<'_, T, P> {
    fn drop(&mut self) {
        self.mutex.raw.unlock_write();
    }
}

pub struct MutexLock<'a, T: ?Sized, P: Platform> {
    mutex: &'a Mutex<T, P>,
    wait: LockWait<'a, P>,
}

impl<'a, T: ?Sized, P: Platform> Future for MutexLock<'a, T, P> {
    type Output = MutexGuard<'a, T, P>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        let wait = unsafe { Pin::new_unchecked(&mut this.wait) };
        wait.poll_lock(cx).map(|()| MutexGuard { mutex: this.mutex, _data: PhantomData })
    }
}
