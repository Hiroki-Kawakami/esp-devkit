/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::time::Duration;

use crate::sync::Queue;
use crate::thread::{MAX_DELAY, ticks};
use crate::{Result, sys};

const QUEUE_TYPE_MUTEX: u8 = 1;

/// A FreeRTOS mutex: priority inheritance, for locks a lower-priority holder
/// must not keep a higher-priority task waiting on.
pub struct Mutex<T: ?Sized> {
    queue: Queue,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        match Self::try_new(value) {
            Ok(m) => m,
            Err(e) => panic!("failed to create mutex: {e}"),
        }
    }

    pub fn try_new(value: T) -> Result<Self> {
        let queue = Queue::new(unsafe { sys::xQueueCreateMutex(QUEUE_TYPE_MUTEX) })?;
        Ok(Self { queue, data: UnsafeCell::new(value) })
    }

    pub fn into_inner(self) -> T {
        let Self { queue, data } = self;
        drop(queue);
        data.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        match self.lock_ticks(MAX_DELAY) {
            Some(guard) => guard,
            None => unreachable!(),
        }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.lock_ticks(0)
    }

    pub fn lock_timeout(&self, timeout: Duration) -> Option<MutexGuard<'_, T>> {
        self.lock_ticks(ticks(timeout))
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    fn lock_ticks(&self, ticks: sys::TickType_t) -> Option<MutexGuard<'_, T>> {
        self.queue.take(ticks).then(|| MutexGuard { mutex: self, _not_send: PhantomData })
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// A FreeRTOS mutex must be given back by the task that took it.
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
    _not_send: PhantomData<*const ()>,
}

unsafe impl<T: ?Sized + Sync> Sync for MutexGuard<'_, T> {}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.queue.send(ptr::null(), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::Mutex;
    use crate::thread::spawn;
    use crate::time::Instant;
    use alloc::sync::Arc;
    use core::time::Duration;

    #[test]
    fn mutex_excludes() {
        let m = Arc::new(Mutex::new(0u32));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let m = m.clone();
                spawn(move || {
                    for _ in 0..1000 {
                        *m.lock() += 1;
                    }
                })
            })
            .collect();
        for h in handles {
            h.join();
        }
        assert_eq!(*m.lock(), 4000);
    }

    #[test]
    fn mutex_try_and_timeout() {
        let m = Arc::new(Mutex::new(1));
        let guard = m.lock();
        let other = m.clone();
        let h = spawn(move || {
            assert!(other.try_lock().is_none());
            let start = Instant::now();
            assert!(other.lock_timeout(Duration::from_millis(20)).is_none());
            assert!(start.elapsed() >= Duration::from_millis(20));
        });
        h.join();
        drop(guard);
        assert_eq!(*m.try_lock().unwrap(), 1);
        let mut m = Arc::try_unwrap(m).ok().unwrap();
        *m.get_mut() = 5;
        assert_eq!(m.into_inner(), 5);
    }
}
