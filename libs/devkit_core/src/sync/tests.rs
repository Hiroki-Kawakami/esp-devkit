/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::cell::Cell;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use super::{Platform, raw::RawLock};

struct Std;

thread_local! {
    static ME: &'static Thread = Box::leak(Box::new(thread::current()));
    static PRIORITY: Cell<u32> = const { Cell::new(0) };
}

static CRITICAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Platform for Std {
    fn current() -> usize {
        ME.with(|me| *me as *const Thread as usize)
    }
    fn priority() -> u32 {
        PRIORITY.with(Cell::get)
    }
    fn running_elsewhere(_task: usize) -> bool {
        true
    }
    fn park() {
        thread::park();
    }
    fn hold(_task: usize) {}
    fn unpark(task: usize) {
        unsafe { &*(task as *const Thread) }.unpark();
    }
    fn now_us() -> u32 {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_micros() as u32
    }
    fn critical<R>(f: impl FnOnce() -> R) -> R {
        let _guard = CRITICAL.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }
}

type Mutex<T> = super::Mutex<T, Std>;
type RwLock<T> = super::RwLock<T, Std>;

fn raw<T>(m: &Mutex<T>) -> &RawLock<Std> {
    &m.raw
}

fn wait_for_waiters(raw: &RawLock<Std>, n: usize) {
    while raw.waiters() != n {
        thread::sleep(Duration::from_micros(100));
    }
}

struct Count(AtomicUsize, Thread);

impl Wake for Count {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
        self.1.unpark();
    }
}

fn counter() -> (Arc<Count>, Waker) {
    let count = Arc::new(Count(AtomicUsize::new(0), thread::current()));
    (count.clone(), Waker::from(count))
}

fn block_on<F: Future>(future: F) -> F::Output {
    let (_, waker) = counter();
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park();
    }
}

#[test]
fn mutex_excludes_tasks() {
    static M: Mutex<u64> = Mutex::new(0);
    let handles: Vec<_> = (0..4)
        .map(|_| {
            thread::spawn(|| {
                for _ in 0..20_000 {
                    *M.lock() += 1;
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*M.lock(), 80_000);
}

#[test]
fn mutex_excludes_tasks_and_futures() {
    let m = Arc::new(Mutex::new(0u64));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let m = m.clone();
            thread::spawn(move || {
                for _ in 0..10_000 {
                    if i % 2 == 0 {
                        *m.lock() += 1;
                    } else {
                        *block_on(m.lock_async()) += 1;
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(Arc::try_unwrap(m).ok().unwrap().into_inner(), 40_000);
}

#[test]
fn try_lock_get_mut_and_default() {
    let mut m = Mutex::<u32>::default();
    let guard = m.try_lock().unwrap();
    assert!(m.try_lock().is_none());
    drop(guard);
    *m.get_mut() = 3;
    assert_eq!(*m.try_lock().unwrap(), 3);
}

#[test]
fn guard_unlocks_from_another_task() {
    let m = Arc::new(Mutex::new(1));
    let guard = m.lock();
    thread::scope(|s| {
        s.spawn(move || drop(guard));
    });
    assert!(m.try_lock().is_some());
}

#[test]
fn waiters_wake_in_priority_order() {
    let m = Arc::new(Mutex::new(Vec::new()));
    let guard = m.lock();
    let handles: Vec<_> = [1u32, 3, 2]
        .into_iter()
        .enumerate()
        .map(|(i, priority)| {
            let shared = m.clone();
            let h = thread::spawn(move || {
                PRIORITY.with(|p| p.set(priority));
                shared.lock().push(priority);
            });
            wait_for_waiters(raw(&m), i + 1);
            h
        })
        .collect();
    drop(guard);
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*m.lock(), [3, 2, 1]);
}

#[test]
fn long_waiter_gets_the_lock_handed_over() {
    let m = Arc::new(Mutex::new(0));
    let guard = m.lock();
    let h = {
        let m = m.clone();
        thread::spawn(move || {
            *m.lock() += 1;
        })
    };
    wait_for_waiters(raw(&m), 1);
    thread::sleep(Duration::from_millis(3));
    drop(guard);
    assert!(m.try_lock().is_none());
    h.join().unwrap();
    assert_eq!(*m.lock(), 1);
}

#[test]
fn async_lock_waits_and_wakes() {
    let m = Mutex::new(5);
    let (count, waker) = counter();
    let mut cx = Context::from_waker(&waker);
    let guard = m.lock();
    let mut lock = pin!(m.lock_async());
    assert!(lock.as_mut().poll(&mut cx).is_pending());
    assert!(lock.as_mut().poll(&mut cx).is_pending());
    drop(guard);
    assert_eq!(count.0.load(Ordering::SeqCst), 1);
    let Poll::Ready(guard) = lock.as_mut().poll(&mut cx) else { panic!("not ready") };
    assert_eq!(*guard, 5);
}

#[test]
fn dropped_lock_future_passes_the_lock_on() {
    let m = Mutex::new(0);
    let (first_count, first) = counter();
    let (second_count, second) = counter();
    let guard = m.lock();
    let mut a = Box::pin(m.lock_async());
    let mut b = Box::pin(m.lock_async());
    assert!(a.as_mut().poll(&mut Context::from_waker(&first)).is_pending());
    assert!(b.as_mut().poll(&mut Context::from_waker(&second)).is_pending());
    thread::sleep(Duration::from_millis(3));
    drop(guard);
    assert_eq!(first_count.0.load(Ordering::SeqCst), 1);
    drop(a);
    assert_eq!(second_count.0.load(Ordering::SeqCst), 1);
    assert!(b.as_mut().poll(&mut Context::from_waker(&second)).is_ready());
}

#[test]
fn queued_future_dropped_leaves_the_queue() {
    let m = Mutex::new(0);
    let (_, waker) = counter();
    let guard = m.lock();
    let mut a = Box::pin(m.lock_async());
    assert!(a.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
    assert_eq!(raw(&m).waiters(), 1);
    drop(a);
    assert_eq!(raw(&m).waiters(), 0);
    drop(guard);
    assert!(m.try_lock().is_some());
}

#[test]
fn readers_share_and_writers_exclude() {
    let l = Arc::new(RwLock::new(0u64));
    let r1 = l.read();
    let r2 = l.try_read().unwrap();
    assert!(l.try_write().is_none());
    drop((r1, r2));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let l = l.clone();
            thread::spawn(move || {
                for _ in 0..10_000 {
                    if i == 0 {
                        let mut w = l.write();
                        *w += 1;
                        *w += 1;
                    } else if i == 1 {
                        let mut w = block_on(l.write_async());
                        *w += 2;
                    } else {
                        let r = if i == 2 { l.read() } else { block_on(l.read_async()) };
                        assert_eq!(*r % 2, 0);
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(*l.read(), 40_000);
}

#[test]
fn waiting_writer_keeps_new_readers_out() {
    let l = Arc::new(RwLock::new(0));
    let reader = l.read();
    let h = {
        let l = l.clone();
        thread::spawn(move || *l.write() = 1)
    };
    wait_for_waiters(&l.raw, 1);
    assert!(l.try_read().is_none());
    drop(reader);
    h.join().unwrap();
    assert_eq!(*l.read(), 1);
}

#[test]
fn writer_release_wakes_every_queued_reader() {
    let l = Arc::new(RwLock::new(0));
    let writer = l.write();
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let l = l.clone();
            thread::spawn(move || *l.read())
        })
        .collect();
    wait_for_waiters(&l.raw, 6);
    drop(writer);
    for h in handles {
        assert_eq!(h.join().unwrap(), 0);
    }
}
