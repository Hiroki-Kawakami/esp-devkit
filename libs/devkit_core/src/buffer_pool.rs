/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut, Range};
use core::ptr::NonNull;

use crate::sync::Platform;

/// `N` bytes on a 64-byte boundary, for a [`BufferPool`] to lend.
#[repr(C, align(64))]
pub struct BufferBytes<const N: usize>([u8; N]);

impl<const N: usize> BufferBytes<N> {
    pub const fn new() -> Self {
        Self([0; N])
    }
}

impl<const N: usize> Default for BufferBytes<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> AsMut<[u8]> for BufferBytes<N> {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

/// The bytes of a `T`, lent out first fit in up to `N` pieces at a time.
/// `T::as_mut` runs inside `P::critical`.
pub struct BufferPool<T, const N: usize, P: Platform> {
    table: UnsafeCell<Table<N>>,
    value: UnsafeCell<T>,
    _platform: PhantomData<P>,
}

struct Table<const N: usize> {
    base: NonNull<u8>,
    len: usize,
    used: [Option<Range<usize>>; N],
}

unsafe impl<T: Send, const N: usize, P: Platform> Send for BufferPool<T, N, P> {}
unsafe impl<T: Send, const N: usize, P: Platform> Sync for BufferPool<T, N, P> {}

impl<T: AsMut<[u8]>, const N: usize, P: Platform> BufferPool<T, N, P> {
    pub const fn new(value: T) -> Self {
        Self {
            table: UnsafeCell::new(Table { base: NonNull::dangling(), len: 0, used: [const { None }; N] }),
            value: UnsafeCell::new(value),
            _platform: PhantomData,
        }
    }

    /// `len` bytes at a multiple of `align`; `None` while no gap holds them or
    /// `N` pieces are out. Never waits.
    pub fn alloc(&self, len: usize, align: usize) -> Option<BufferLease<'_>>
    where
        T: Send,
    {
        let (slot, ptr) = P::critical(|| {
            let table = unsafe { &mut *self.table.get() };
            if table.used.iter().all(Option::is_none) {
                let bytes = unsafe { (*self.value.get()).as_mut() };
                table.len = bytes.len();
                table.base = NonNull::from(bytes).cast();
            }
            let slot = table.used.iter().position(Option::is_none)?;
            let start = table.fit(len, align.max(1))?;
            table.used[slot] = Some(start..start + len);
            Some((slot, unsafe { table.base.add(start) }))
        })?;
        Some(BufferLease { ptr, len, slot, pool: self })
    }
}

impl<const N: usize> Table<N> {
    fn fit(&self, len: usize, align: usize) -> Option<usize> {
        let base = self.base.as_ptr() as usize;
        let mut start = 0;
        loop {
            start = (base + start).checked_next_multiple_of(align)? - base;
            let end = start.checked_add(len)?;
            if end > self.len {
                return None;
            }
            match self.used.iter().flatten().filter(|r| r.start < end && start < r.end).map(|r| r.end).max() {
                Some(after) => start = after,
                None => return Some(start),
            }
        }
    }
}

trait Free: Sync {
    fn free(&self, slot: usize);
}

impl<T: Send, const N: usize, P: Platform> Free for BufferPool<T, N, P> {
    fn free(&self, slot: usize) {
        P::critical(|| unsafe { (*self.table.get()).used[slot] = None });
    }
}

/// Bytes of a [`BufferPool`], back in it once dropped.
pub struct BufferLease<'a> {
    ptr: NonNull<u8>,
    len: usize,
    slot: usize,
    pool: &'a dyn Free,
}

unsafe impl Send for BufferLease<'_> {}
unsafe impl Sync for BufferLease<'_> {}

impl Deref for BufferLease<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for BufferLease<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl AsRef<[u8]> for BufferLease<'_> {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl AsMut<[u8]> for BufferLease<'_> {
    fn as_mut(&mut self) -> &mut [u8] {
        self
    }
}

impl Drop for BufferLease<'_> {
    fn drop(&mut self) {
        self.pool.free(self.slot);
    }
}

impl core::fmt::Debug for BufferLease<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "BufferLease({} bytes)", self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::{BufferBytes, BufferPool};
    use crate::sync::Platform;

    struct Std;

    static CRITICAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl Platform for Std {
        fn current() -> usize {
            0
        }
        fn priority() -> u32 {
            0
        }
        fn running_elsewhere(_task: usize) -> bool {
            false
        }
        fn park() {}
        fn hold(_task: usize) {}
        fn unpark(_task: usize) {}
        fn now_us() -> u32 {
            0
        }
        fn critical<R>(f: impl FnOnce() -> R) -> R {
            let _guard = CRITICAL.lock().unwrap_or_else(|e| e.into_inner());
            f()
        }
    }

    type Pool = BufferPool<BufferBytes<256>, 3, Std>;

    fn offset(pool: &Pool, lease: &[u8]) -> usize {
        lease.as_ptr() as usize - unsafe { (*pool.value.get()).0.as_ptr() } as usize
    }

    #[test]
    fn first_fit_reuses_a_freed_gap() {
        let pool = Pool::new(BufferBytes::new());
        let front = pool.alloc(160, 64).unwrap();
        let ring = pool.alloc(64, 1).unwrap();
        let record = pool.alloc(32, 1).unwrap();
        assert_eq!([offset(&pool, &front), offset(&pool, &ring), offset(&pool, &record)], [0, 160, 224]);
        assert!(pool.alloc(256, 64).is_none());
        drop(front);
        let mirror = pool.alloc(256, 64).or_else(|| pool.alloc(160, 64)).unwrap();
        assert_eq!((offset(&pool, &mirror), mirror.len()), (0, 160));
        drop((mirror, record, ring));
        assert_eq!(pool.alloc(256, 64).unwrap().len(), 256);
    }

    #[test]
    fn aligns_and_counts_pieces() {
        let pool = Pool::new(BufferBytes::new());
        let a = pool.alloc(1, 1).unwrap();
        let b = pool.alloc(1, 64).unwrap();
        assert_eq!(offset(&pool, &b), 64);
        let c = pool.alloc(1, 1).unwrap();
        assert_eq!(offset(&pool, &c), 1);
        assert!(pool.alloc(1, 1).is_none());
        drop(a);
        assert_eq!(offset(&pool, &pool.alloc(0, 1).unwrap()), 0);
        drop((b, c));
    }

    #[test]
    fn keeps_what_was_written() {
        static POOL: Pool = Pool::new(BufferBytes::new());
        let mut lease = POOL.alloc(256, 64).unwrap();
        lease.fill(7);
        drop(lease);
        let lease = std::thread::spawn(|| POOL.alloc(128, 1).unwrap()).join().unwrap();
        assert!(lease.iter().all(|&b| b == 7));
    }
}
