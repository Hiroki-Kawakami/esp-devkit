/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Handing values to async code from tasks and interrupts.
 */

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::future::{Future, poll_fn};
use core::marker::PhantomPinned;
use core::pin::Pin;
use core::ptr;
use core::task::{Context, Poll, Waker};

use super::CriticalSection;

pub(super) fn critical<R>(cs: &dyn CriticalSection, f: impl FnOnce() -> R) -> R {
    let mut f = Some(f);
    let mut out = None;
    cs.with(&mut || out = Some((f.take().unwrap())()));
    out.unwrap()
}

// The stored waker stays where it is: an interrupt only wakes a copy, so it
// never drops the last reference to a task and frees it there.
pub(super) fn wake_copy(waker: Option<Waker>) {
    if let Some(waker) = waker {
        waker.wake_by_ref();
    }
}

/// Holds the latest value until a waiter takes it.
pub struct Signal<T> {
    critical: &'static dyn CriticalSection,
    state: UnsafeCell<(Option<T>, Option<Waker>)>,
}

unsafe impl<T: Send> Send for Signal<T> {}
unsafe impl<T: Send> Sync for Signal<T> {}

impl<T> Signal<T> {
    pub const fn new(critical: &'static dyn CriticalSection) -> Self {
        Self { critical, state: UnsafeCell::new((None, None)) }
    }

    /// Replaces a value nobody took yet. From any task or interrupt; the
    /// value is dropped here when it replaces one, and the waiter's waker
    /// must not allocate when cloned (true for devkit executors).
    pub fn signal(&self, value: T) {
        let mut waker = None;
        let old = critical(self.critical, || {
            let state = unsafe { &mut *self.state.get() };
            waker = state.1.clone();
            state.0.replace(value)
        });
        drop(old);
        wake_copy(waker);
    }

    pub fn try_take(&self) -> Option<T> {
        critical(self.critical, || unsafe { (*self.state.get()).0.take() })
    }

    /// One waiter at a time.
    pub fn wait(&self) -> impl Future<Output = T> + '_ {
        poll_fn(move |cx| {
            let new = cx.waker().clone();
            let mut old = None;
            let value = critical(self.critical, || {
                let state = unsafe { &mut *self.state.get() };
                let value = state.0.take();
                if value.is_none() {
                    old = state.1.replace(new);
                }
                value
            });
            drop(old);
            value.map_or(Poll::Pending, Poll::Ready)
        })
    }
}

struct SendNode {
    waker: Option<Waker>,
    prev: *mut SendNode,
    next: *mut SendNode,
}

struct Queue<T> {
    items: VecDeque<T>,
    capacity: usize,
    receiver: Option<Waker>,
    senders: *mut SendNode,
    closed: bool,
}

struct Chan<T> {
    critical: &'static dyn CriticalSection,
    queue: UnsafeCell<Queue<T>>,
}

unsafe impl<T: Send> Send for Chan<T> {}
unsafe impl<T: Send> Sync for Chan<T> {}

impl<T> Chan<T> {
    /// Inside the critical section.
    #[allow(clippy::mut_from_ref)]
    unsafe fn queue(&self) -> &mut Queue<T> {
        unsafe { &mut *self.queue.get() }
    }

    /// Inside the critical section.
    unsafe fn unlink(&self, node: *mut SendNode) -> Option<Waker> {
        let queue = unsafe { self.queue() };
        let waker = unsafe { (*node).waker.take()? };
        let (prev, next) = unsafe { ((*node).prev, (*node).next) };
        if prev.is_null() {
            queue.senders = next;
        } else {
            unsafe { (*prev).next = next };
        }
        if !next.is_null() {
            unsafe { (*next).prev = prev };
        }
        Some(waker)
    }

    /// Inside the critical section; the value back when full or closed.
    unsafe fn push(&self, value: T) -> Result<Option<Waker>, T> {
        let queue = unsafe { self.queue() };
        if queue.closed || queue.items.len() == queue.capacity {
            return Err(value);
        }
        queue.items.push_back(value);
        Ok(queue.receiver.clone())
    }
}

/// A bounded queue into async code; values move in and out in order.
/// Senders see the receiver go, but `recv` waits with every sender gone.
pub fn channel<T>(critical: &'static dyn CriticalSection, capacity: usize) -> (Sender<T>, Receiver<T>) {
    assert!(capacity > 0, "a channel needs room for one value");
    let chan = Arc::new(Chan {
        critical,
        queue: UnsafeCell::new(Queue {
            items: VecDeque::with_capacity(capacity),
            capacity,
            receiver: None,
            senders: ptr::null_mut(),
            closed: false,
        }),
    });
    (Sender(chan.clone()), Receiver(chan))
}

pub struct Sender<T>(Arc<Chan<T>>);

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Sender<T> {
    /// From any task or interrupt, with the same waker rule as
    /// `Signal::signal`; the value back when the channel is full or the
    /// receiver has gone.
    pub fn try_send(&self, value: T) -> Result<(), T> {
        let chan = &*self.0;
        let waker = critical(chan.critical, || unsafe { chan.push(value) })?;
        wake_copy(waker);
        Ok(())
    }

    /// The receiver has gone.
    pub fn is_closed(&self) -> bool {
        critical(self.0.critical, || unsafe { self.0.queue().closed })
    }

    /// Waits for room; senders get it in the order they started waiting.
    /// Once the receiver has gone it returns at once, dropping the value.
    pub fn send(&self, value: T) -> SendFuture<'_, T> {
        SendFuture {
            chan: &self.0,
            value: Some(value),
            node: UnsafeCell::new(SendNode { waker: None, prev: ptr::null_mut(), next: ptr::null_mut() }),
            _pinned: PhantomPinned,
        }
    }
}

pub struct SendFuture<'a, T> {
    chan: &'a Chan<T>,
    value: Option<T>,
    node: UnsafeCell<SendNode>,
    _pinned: PhantomPinned,
}

unsafe impl<T: Send> Send for SendFuture<'_, T> {}

impl<T> Future for SendFuture<'_, T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = unsafe { self.get_unchecked_mut() };
        let chan = this.chan;
        let node = this.node.get();
        let new = cx.waker().clone();
        let mut value = this.value.take();
        let mut receiver = None;
        let mut next = None;
        let mut old = None;
        let mut refused = None;
        let sent = critical(chan.critical, || unsafe {
            let queue = chan.queue();
            let waiting = (*node).waker.is_some();
            let first = !waiting || queue.senders == node;
            if queue.closed || (first && queue.items.len() < queue.capacity) {
                old = chan.unlink(node);
                match chan.push(value.take().unwrap()) {
                    Ok(waker) => receiver = waker,
                    Err(value) => refused = Some(value),
                }
                if queue.items.len() < queue.capacity && !queue.senders.is_null() {
                    next = (*queue.senders).waker.clone();
                }
                return true;
            }
            old = (*node).waker.replace(new);
            if !waiting {
                (*node).prev = ptr::null_mut();
                (*node).next = ptr::null_mut();
                let mut tail = queue.senders;
                if tail.is_null() {
                    queue.senders = node;
                } else {
                    while !(*tail).next.is_null() {
                        tail = (*tail).next;
                    }
                    (*tail).next = node;
                    (*node).prev = tail;
                }
            }
            false
        });
        drop(old);
        drop(refused);
        if sent {
            wake_copy(receiver);
            wake_copy(next);
            Poll::Ready(())
        } else {
            this.value = value;
            Poll::Pending
        }
    }
}

impl<T> Drop for SendFuture<'_, T> {
    fn drop(&mut self) {
        let chan = self.chan;
        let node = self.node.get();
        let mut next = None;
        let old = critical(chan.critical, || unsafe {
            let was_first = chan.queue().senders == node;
            let old = chan.unlink(node);
            if was_first {
                let head = chan.queue().senders;
                if !head.is_null() {
                    next = (*head).waker.clone();
                }
            }
            old
        });
        drop(old);
        wake_copy(next);
    }
}

/// The one receiving end.
pub struct Receiver<T>(Arc<Chan<T>>);

impl<T> Receiver<T> {
    pub fn try_recv(&self) -> Option<T> {
        let chan = &*self.0;
        let mut sender = None;
        let value = critical(chan.critical, || unsafe {
            let queue = chan.queue();
            let value = queue.items.pop_front();
            if value.is_some() && !queue.senders.is_null() {
                sender = (*queue.senders).waker.clone();
            }
            value
        });
        wake_copy(sender);
        value
    }

    pub async fn recv(&mut self) -> T {
        poll_fn(|cx| {
            if let Some(value) = self.try_recv() {
                return Poll::Ready(value);
            }
            let chan = &*self.0;
            let new = cx.waker().clone();
            let mut old = None;
            let mut sender = None;
            let value = critical(chan.critical, || unsafe {
                let queue = chan.queue();
                let value = queue.items.pop_front();
                if value.is_none() {
                    old = queue.receiver.replace(new);
                } else if !queue.senders.is_null() {
                    sender = (*queue.senders).waker.clone();
                }
                value
            });
            drop(old);
            wake_copy(sender);
            value.map_or(Poll::Pending, Poll::Ready)
        })
        .await
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let chan = &*self.0;
        let mut sender = None;
        let (items, receiver) = critical(chan.critical, || unsafe {
            let queue = chan.queue();
            queue.closed = true;
            if !queue.senders.is_null() {
                sender = (*queue.senders).waker.clone();
            }
            (core::mem::take(&mut queue.items), queue.receiver.take())
        });
        drop(items);
        drop(receiver);
        wake_copy(sender);
    }
}
