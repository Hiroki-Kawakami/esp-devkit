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

/// A writer waiting keeps new readers out.
pub struct RwLock<T: ?Sized, P: Platform> {
    pub(super) raw: RawLock<P>,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send, P: Platform> Send for RwLock<T, P> {}
unsafe impl<T: ?Sized + Send + Sync, P: Platform> Sync for RwLock<T, P> {}

impl<T, P: Platform> RwLock<T, P> {
    pub const fn new(value: T) -> Self {
        Self { raw: RawLock::new(), data: UnsafeCell::new(value) }
    }

    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized, P: Platform> RwLock<T, P> {
    /// Blocks the calling task, and so every future of an executor it runs.
    pub fn read(&self) -> RwLockReadGuard<'_, T, P> {
        self.raw.read();
        RwLockReadGuard { lock: self, _data: PhantomData }
    }

    /// Blocks the calling task, and so every future of an executor it runs.
    pub fn write(&self) -> RwLockWriteGuard<'_, T, P> {
        self.raw.write();
        RwLockWriteGuard { lock: self, _data: PhantomData }
    }

    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T, P>> {
        self.raw.try_read().then(|| RwLockReadGuard { lock: self, _data: PhantomData })
    }

    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T, P>> {
        self.raw.try_write().then(|| RwLockWriteGuard { lock: self, _data: PhantomData })
    }

    pub fn read_async(&self) -> RwLockRead<'_, T, P> {
        RwLockRead { lock: self, wait: LockWait::new(&self.raw, true) }
    }

    pub fn write_async(&self) -> RwLockWrite<'_, T, P> {
        RwLockWrite { lock: self, wait: LockWait::new(&self.raw, false) }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

impl<T: Default, P: Platform> Default for RwLock<T, P> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

pub struct RwLockReadGuard<'a, T: ?Sized, P: Platform> {
    lock: &'a RwLock<T, P>,
    _data: PhantomData<&'a T>,
}

impl<T: ?Sized, P: Platform> Deref for RwLockReadGuard<'_, T, P> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized, P: Platform> Drop for RwLockReadGuard<'_, T, P> {
    fn drop(&mut self) {
        self.lock.raw.unlock_read();
    }
}

pub struct RwLockWriteGuard<'a, T: ?Sized, P: Platform> {
    lock: &'a RwLock<T, P>,
    _data: PhantomData<&'a mut T>,
}

impl<T: ?Sized, P: Platform> Deref for RwLockWriteGuard<'_, T, P> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized, P: Platform> DerefMut for RwLockWriteGuard<'_, T, P> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized, P: Platform> Drop for RwLockWriteGuard<'_, T, P> {
    fn drop(&mut self) {
        self.lock.raw.unlock_write();
    }
}

pub struct RwLockRead<'a, T: ?Sized, P: Platform> {
    lock: &'a RwLock<T, P>,
    wait: LockWait<'a, P>,
}

impl<'a, T: ?Sized, P: Platform> Future for RwLockRead<'a, T, P> {
    type Output = RwLockReadGuard<'a, T, P>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        let wait = unsafe { Pin::new_unchecked(&mut this.wait) };
        wait.poll_lock(cx).map(|()| RwLockReadGuard { lock: this.lock, _data: PhantomData })
    }
}

pub struct RwLockWrite<'a, T: ?Sized, P: Platform> {
    lock: &'a RwLock<T, P>,
    wait: LockWait<'a, P>,
}

impl<'a, T: ?Sized, P: Platform> Future for RwLockWrite<'a, T, P> {
    type Output = RwLockWriteGuard<'a, T, P>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        let wait = unsafe { Pin::new_unchecked(&mut this.wait) };
        wait.poll_lock(cx).map(|()| RwLockWriteGuard { lock: this.lock, _data: PhantomData })
    }
}
