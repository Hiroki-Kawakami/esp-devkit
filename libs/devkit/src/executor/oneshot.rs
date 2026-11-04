/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use super::CriticalSection;
use super::sync::{critical, wake_copy};

struct State<T> {
    value: Option<T>,
    waker: Option<Waker>,
    sender: bool,
    receiver: bool,
}

struct Shot<T> {
    critical: &'static dyn CriticalSection,
    state: UnsafeCell<State<T>>,
}

unsafe impl<T: Send> Send for Shot<T> {}
unsafe impl<T: Send> Sync for Shot<T> {}

impl<T> Shot<T> {
    fn with<R>(&self, f: impl FnOnce(&mut State<T>) -> R) -> R {
        critical(self.critical, || f(unsafe { &mut *self.state.get() }))
    }
}

/// One value from any task to async code. Each side sees the other go.
pub fn channel<T>(critical: &'static dyn CriticalSection) -> (Sender<T>, Receiver<T>) {
    let shot = Arc::new(Shot {
        critical,
        state: UnsafeCell::new(State { value: None, waker: None, sender: true, receiver: true }),
    });
    (Sender(shot.clone()), Receiver(shot))
}

/// The sender went without sending.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Closed;

pub struct Sender<T>(Arc<Shot<T>>);

impl<T> Sender<T> {
    /// The value back once the receiver has gone.
    pub fn send(self, value: T) -> Result<(), T> {
        let mut waker = None;
        let refused = self.0.with(|state| {
            if !state.receiver {
                return Some(value);
            }
            state.value = Some(value);
            waker = state.waker.clone();
            None
        });
        if let Some(value) = refused {
            return Err(value);
        }
        wake_copy(waker);
        Ok(())
    }

    /// Nothing waits for the value any more.
    pub fn is_closed(&self) -> bool {
        self.0.with(|state| !state.receiver)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let waker = self.0.with(|state| {
            state.sender = false;
            state.waker.clone()
        });
        wake_copy(waker);
    }
}

/// The value, or `Closed`; dropping it closes the channel.
pub struct Receiver<T>(Arc<Shot<T>>);

impl<T> Future for Receiver<T> {
    type Output = Result<T, Closed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<T, Closed>> {
        let new = cx.waker().clone();
        let mut old = None;
        let ready = self.0.with(|state| {
            if let Some(value) = state.value.take() {
                return Some(Ok(value));
            }
            if !state.sender {
                return Some(Err(Closed));
            }
            old = state.waker.replace(new);
            None
        });
        drop(old);
        ready.map_or(Poll::Pending, Poll::Ready)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let (value, waker) = self.0.with(|state| {
            state.receiver = false;
            (state.value.take(), state.waker.take())
        });
        drop(value);
        drop(waker);
    }
}
