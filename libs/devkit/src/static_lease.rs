/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

pub struct StaticLease<T> {
    taken: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for StaticLease<T> {}

impl<T> StaticLease<T> {
    pub const fn new(value: T) -> Self {
        Self { taken: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    pub fn take(&self) -> Option<Lease<'_, T>> {
        self.taken
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Lease { owner: self, _value: PhantomData })
    }
}

pub struct Lease<'a, T> {
    owner: &'a StaticLease<T>,
    _value: PhantomData<&'a mut T>,
}

impl<T> Deref for Lease<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.owner.value.get() }
    }
}

impl<T> DerefMut for Lease<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.owner.value.get() }
    }
}

impl<T> Drop for Lease<'_, T> {
    fn drop(&mut self) {
        self.owner.taken.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::StaticLease;

    #[repr(C, align(64))]
    struct Sram([u8; 256]);

    static SRAM: StaticLease<Sram> = StaticLease::new(Sram([0; 256]));

    #[test]
    fn one_lease_at_a_time() {
        let mut lease = SRAM.take().unwrap();
        assert!(SRAM.take().is_none());
        assert_eq!(lease.0.as_ptr() as usize % 64, 0);
        let (a, b) = lease.0.split_at_mut(128);
        a.fill(1);
        b.fill(2);
        drop(lease);

        let lease = SRAM.take().unwrap();
        assert_eq!(lease.0[0], 1);
        assert_eq!(lease.0[255], 2);
    }

    #[test]
    fn contended_take() {
        static COUNTER: StaticLease<u64> = StaticLease::new(0);
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    let mut done = 0;
                    while done < 1000 {
                        if let Some(mut c) = COUNTER.take() {
                            *c += 1;
                            done += 1;
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(*COUNTER.take().unwrap(), 4000);
    }
}
