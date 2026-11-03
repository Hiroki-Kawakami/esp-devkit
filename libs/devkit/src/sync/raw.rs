/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::cell::UnsafeCell;
use core::marker::{PhantomData, PhantomPinned};
use core::pin::Pin;
use core::ptr;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use core::task::{Context, Poll, Waker};

use super::Platform;

const WRITER: usize = 1;
// The flags change only inside the critical section; outside it the state
// changes only by taking or dropping WRITER and readers.
const PARKED: usize = 2;
const WRITER_PARKED: usize = 4;
const ONE_READER: usize = 8;
const READERS: usize = !(ONE_READER - 1);

const SPIN_LIMIT: u32 = 1000;
// A waiter queued this long gets the lock handed to it instead of racing new
// lockers for it.
const HANDOFF_US: u32 = 1000;
const BATCH: usize = 4;

const WAITING: u8 = 0;
const RETRY: u8 = 1;
const GRANTED: u8 = 2;

enum Wake {
    Task(usize),
    Waker(Option<Waker>),
}

impl Wake {
    fn wake<P: Platform>(self) {
        match self {
            Wake::Task(task) => P::unpark(task),
            Wake::Waker(waker) => {
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
        }
    }
}

/// Fields other than `state` belong to the critical section while linked.
struct Waiter {
    next: *mut Waiter,
    prev: *mut Waiter,
    linked: bool,
    shared: bool,
    priority: u32,
    since: u32,
    wake: Wake,
    state: AtomicU8,
}

impl Waiter {
    const fn new(shared: bool, wake: Wake) -> Self {
        Self {
            next: ptr::null_mut(),
            prev: ptr::null_mut(),
            linked: false,
            shared,
            priority: 0,
            since: 0,
            wake,
            state: AtomicU8::new(WAITING),
        }
    }
}

/// Ordered by priority, first come first within one.
struct Queue {
    head: *mut Waiter,
    tail: *mut Waiter,
    writers: usize,
}

pub(super) struct RawLock<P> {
    state: AtomicUsize,
    owner: AtomicUsize,
    queue: UnsafeCell<Queue>,
    _platform: PhantomData<fn() -> P>,
}

unsafe impl<P> Send for RawLock<P> {}
unsafe impl<P> Sync for RawLock<P> {}

impl<P: Platform> RawLock<P> {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            owner: AtomicUsize::new(0),
            queue: UnsafeCell::new(Queue { head: ptr::null_mut(), tail: ptr::null_mut(), writers: 0 }),
            _platform: PhantomData,
        }
    }

    pub(super) fn try_write(&self) -> bool {
        let mut s = self.state.load(Ordering::Relaxed);
        loop {
            if s & (WRITER | READERS) != 0 {
                return false;
            }
            match self.state.compare_exchange_weak(s, s | WRITER, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => {
                    self.owner.store(P::current(), Ordering::Relaxed);
                    return true;
                }
                Err(now) => s = now,
            }
        }
    }

    pub(super) fn try_read(&self) -> bool {
        let mut s = self.state.load(Ordering::Relaxed);
        loop {
            if s & (WRITER | WRITER_PARKED) != 0 {
                return false;
            }
            match self.state.compare_exchange_weak(s, s + ONE_READER, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => return true,
                Err(now) => s = now,
            }
        }
    }

    fn try_lock(&self, shared: bool) -> bool {
        if shared { self.try_read() } else { self.try_write() }
    }

    pub(super) fn write(&self) {
        if !self.try_write() {
            self.wait(false);
            self.owner.store(P::current(), Ordering::Relaxed);
        }
    }

    pub(super) fn read(&self) {
        if !self.try_read() {
            self.wait(true);
        }
    }

    pub(super) fn unlock_write(&self) {
        if self.state.compare_exchange(WRITER, 0, Ordering::Release, Ordering::Relaxed).is_err() {
            self.release(true);
        }
    }

    pub(super) fn unlock_read(&self) {
        let prev = self.state.fetch_sub(ONE_READER, Ordering::Release);
        if prev & READERS == ONE_READER && prev & PARKED != 0 {
            self.release(false);
        }
    }

    fn unlock(&self, shared: bool) {
        if shared { self.unlock_read() } else { self.unlock_write() }
    }

    // Worth it only while the writer runs on another core: preempted, it
    // cannot let go before this task blocks.
    fn spinnable(&self) -> bool {
        if self.state.load(Ordering::Relaxed) & WRITER == 0 {
            return false;
        }
        let owner = self.owner.load(Ordering::Relaxed);
        owner != 0 && P::running_elsewhere(owner)
    }

    fn wait(&self, shared: bool) {
        for _ in 0..SPIN_LIMIT {
            if !self.spinnable() {
                break;
            }
            core::hint::spin_loop();
            if self.try_lock(shared) {
                return;
            }
        }
        let waiter = UnsafeCell::new(Waiter::new(shared, Wake::Task(P::current())));
        let w = waiter.get();
        unsafe {
            (*w).priority = P::priority();
            (*w).since = P::now_us();
        }
        loop {
            if P::critical(|| unsafe { self.enqueue(w) }) {
                return;
            }
            loop {
                match unsafe { (*w).state.load(Ordering::Acquire) } {
                    WAITING => P::park(),
                    GRANTED => return,
                    _ => break,
                }
            }
            unsafe { (*w).state.store(WAITING, Ordering::Relaxed) };
        }
    }

    /// Inside the critical section. Takes the lock if it is free for `w`,
    /// else queues `w`.
    unsafe fn enqueue(&self, w: *mut Waiter) -> bool {
        let shared = unsafe { (*w).shared };
        let flags = if shared { PARKED } else { PARKED | WRITER_PARKED };
        let mut s = self.state.load(Ordering::Relaxed);
        loop {
            let free = if shared { s & (WRITER | WRITER_PARKED) == 0 } else { s & (WRITER | READERS) == 0 };
            let new = match (free, shared) {
                (true, true) => s + ONE_READER,
                (true, false) => s | WRITER,
                (false, _) => s | flags,
            };
            match self.state.compare_exchange_weak(s, new, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) if free => return true,
                Ok(_) => break,
                Err(now) => s = now,
            }
        }
        let queue = unsafe { &mut *self.queue.get() };
        unsafe {
            let mut after = queue.tail;
            while !after.is_null() && (*after).priority < (*w).priority {
                after = (*after).prev;
            }
            let before = if after.is_null() { queue.head } else { (*after).next };
            (*w).prev = after;
            (*w).next = before;
            if after.is_null() {
                queue.head = w;
            } else {
                (*after).next = w;
            }
            if before.is_null() {
                queue.tail = w;
            } else {
                (*before).prev = w;
            }
            (*w).linked = true;
        }
        if !shared {
            queue.writers += 1;
        }
        false
    }

    /// Inside the critical section.
    unsafe fn unlink(&self, w: *mut Waiter) {
        let queue = unsafe { &mut *self.queue.get() };
        unsafe {
            let (prev, next) = ((*w).prev, (*w).next);
            if prev.is_null() {
                queue.head = next;
            } else {
                (*prev).next = next;
            }
            if next.is_null() {
                queue.tail = prev;
            } else {
                (*next).prev = prev;
            }
            (*w).linked = false;
            if !(*w).shared {
                queue.writers -= 1;
            }
        }
    }

    /// Inside the critical section; the flags the queue calls for.
    fn flags(&self) -> usize {
        let queue = unsafe { &*self.queue.get() };
        match (queue.head.is_null(), queue.writers) {
            (true, _) => 0,
            (false, 0) => PARKED,
            (false, _) => PARKED | WRITER_PARKED,
        }
    }

    /// Drops WRITER when `clear`, then wakes whoever may take the lock now.
    fn release(&self, clear: bool) {
        let mut clear = clear;
        loop {
            let mut wakes = [const { None }; BATCH];
            let more = P::critical(|| unsafe { self.grant(&mut clear, &mut wakes) });
            for wake in wakes.into_iter().flatten() {
                wake.wake::<P>();
            }
            if !more {
                return;
            }
        }
    }

    /// Inside the critical section. A queued writer goes first; otherwise up
    /// to `BATCH` readers, and true when more readers are left.
    unsafe fn grant(&self, clear: &mut bool, wakes: &mut [Option<Wake>; BATCH]) -> bool {
        let now = P::now_us();
        let queue = unsafe { &mut *self.queue.get() };
        let mut picked = [ptr::null_mut::<Waiter>(); BATCH];
        let mut handoff = [false; BATCH];
        let mut s = self.state.load(Ordering::Relaxed);
        let count = loop {
            let held = if *clear { s & !WRITER } else { s };
            let mut count = 0;
            let mut granted = 0;
            let mut writers = queue.writers;
            if writers > 0 {
                if held & (WRITER | READERS) == 0 {
                    let mut w = queue.head;
                    while unsafe { (*w).shared } {
                        w = unsafe { (*w).next };
                    }
                    picked[0] = w;
                    handoff[0] = now.wrapping_sub(unsafe { (*w).since }) >= HANDOFF_US;
                    if handoff[0] {
                        granted = WRITER;
                    }
                    count = 1;
                    writers -= 1;
                }
            } else if held & WRITER == 0 {
                let mut w = queue.head;
                while !w.is_null() && count < BATCH {
                    picked[count] = w;
                    handoff[count] = now.wrapping_sub(unsafe { (*w).since }) >= HANDOFF_US;
                    if handoff[count] {
                        granted += ONE_READER;
                    }
                    count += 1;
                    w = unsafe { (*w).next };
                }
            }
            let mut flags = 0;
            if count < self.queued() {
                flags = if writers > 0 { PARKED | WRITER_PARKED } else { PARKED };
            }
            let new = ((held & !(PARKED | WRITER_PARKED)) + granted) | flags;
            match self.state.compare_exchange_weak(s, new, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => break count,
                Err(now) => s = now,
            }
        };
        *clear = false;
        for i in 0..count {
            let w = picked[i];
            unsafe {
                self.unlink(w);
                wakes[i] = Some(match &mut (*w).wake {
                    Wake::Task(task) => {
                        P::hold(*task);
                        Wake::Task(*task)
                    }
                    Wake::Waker(waker) => Wake::Waker(waker.take()),
                });
                (*w).state.store(if handoff[i] { GRANTED } else { RETRY }, Ordering::Release);
            }
        }
        count == BATCH && !queue.head.is_null() && queue.writers == 0
    }

    /// Inside the critical section.
    fn queued(&self) -> usize {
        let mut n = 0;
        let mut w = unsafe { (*self.queue.get()).head };
        while !w.is_null() {
            n += 1;
            w = unsafe { (*w).next };
        }
        n
    }

    #[cfg(test)]
    pub(super) fn waiters(&self) -> usize {
        P::critical(|| self.queued())
    }
}

/// The async side of `RawLock::read`/`write`.
pub(super) struct LockWait<'a, P: Platform> {
    lock: &'a RawLock<P>,
    waiter: UnsafeCell<Waiter>,
    queued: bool,
    started: bool,
    _pinned: PhantomPinned,
}

unsafe impl<P: Platform> Send for LockWait<'_, P> {}

impl<'a, P: Platform> LockWait<'a, P> {
    pub(super) const fn new(lock: &'a RawLock<P>, shared: bool) -> Self {
        Self {
            lock,
            waiter: UnsafeCell::new(Waiter::new(shared, Wake::Waker(None))),
            queued: false,
            started: false,
            _pinned: PhantomPinned,
        }
    }

    pub(super) fn poll_lock(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = unsafe { self.get_unchecked_mut() };
        let lock = this.lock;
        let w = this.waiter.get();
        let shared = unsafe { (*w).shared };
        if this.queued {
            loop {
                match unsafe { (*w).state.load(Ordering::Acquire) } {
                    GRANTED => {
                        this.queued = false;
                        if !shared {
                            lock.owner.store(P::current(), Ordering::Relaxed);
                        }
                        return Poll::Ready(());
                    }
                    RETRY => {
                        this.queued = false;
                        unsafe { (*w).state.store(WAITING, Ordering::Relaxed) };
                        break;
                    }
                    _ => {}
                }
                let mut old = None;
                let linked = P::critical(|| unsafe {
                    if (*w).linked
                        && let Wake::Waker(slot) = &mut (*w).wake
                        && !slot.as_ref().is_some_and(|waker| waker.will_wake(cx.waker()))
                    {
                        old = slot.replace(cx.waker().clone());
                    }
                    (*w).linked
                });
                drop(old);
                if linked {
                    return Poll::Pending;
                }
            }
        }
        if lock.try_lock(shared) {
            return Poll::Ready(());
        }
        unsafe {
            if !this.started {
                this.started = true;
                (*w).priority = P::priority();
                (*w).since = P::now_us();
            }
            (*w).wake = Wake::Waker(Some(cx.waker().clone()));
        }
        if P::critical(|| unsafe { lock.enqueue(w) }) {
            unsafe { (*w).wake = Wake::Waker(None) };
            if !shared {
                lock.owner.store(P::current(), Ordering::Relaxed);
            }
            return Poll::Ready(());
        }
        this.queued = true;
        Poll::Pending
    }
}

impl<P: Platform> Drop for LockWait<'_, P> {
    fn drop(&mut self) {
        if !self.queued {
            return;
        }
        let lock = self.lock;
        let w = self.waiter.get();
        let shared = unsafe { (*w).shared };
        let mut old = None;
        let unlinked = P::critical(|| unsafe {
            if !(*w).linked {
                return false;
            }
            lock.unlink(w);
            let flags = lock.flags();
            lock.state.fetch_and(!(PARKED | WRITER_PARKED) | flags, Ordering::Relaxed);
            if let Wake::Waker(slot) = &mut (*w).wake {
                old = slot.take();
            }
            true
        });
        drop(old);
        // Passed what was meant for this waiter on: the lock handed to it, the
        // wakeup it would have retried on, or the readers a writer held back.
        if unlinked {
            if !shared {
                lock.release(false);
            }
            return;
        }
        match unsafe { (*w).state.load(Ordering::Acquire) } {
            GRANTED => lock.unlock(shared),
            _ => lock.release(false),
        }
    }
}
