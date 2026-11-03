/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::{Poll, Waker};

use idf::heap::Caps;
use idf::sync::{Mutex, MutexGuard, OnceLock};

use crate::link::{Link, LinkInfo};
use crate::stack::{IfaceId, SocketId, Stack, Tx};
use crate::time::Instant;
use crate::{Error, Result};

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Receive buffer of a UDP socket that sets none of its own.
    pub udp_rx_buffer: usize,
    pub tcp_rx_buffer: usize,
    pub tcp_tx_buffer: usize,
    /// Where socket buffers are allocated unless a socket says otherwise.
    pub caps: Caps,
}

impl Default for Config {
    fn default() -> Self {
        Self { udp_rx_buffer: 4096, tcp_rx_buffer: 16384, tcp_tx_buffer: 16384, caps: Caps::DEFAULT }
    }
}

pub(crate) struct Inner {
    pub stack: Stack,
    links: Vec<Option<Arc<dyn Link>>>,
    waiters: Vec<(SocketId, Waker)>,
}

impl Inner {
    pub fn with_tx<R>(&mut self, f: impl FnOnce(&mut Stack, &mut Tx) -> R) -> R {
        let links = &self.links;
        let result = f(&mut self.stack, &mut |id, frame| {
            links.get(id.index()).and_then(Option::as_ref).is_some_and(|l| l.transmit(frame).is_ok())
        });
        self.wake();
        result
    }

    fn wake(&mut self) {
        for socket in self.stack.woken.drain(..) {
            self.waiters.retain(|(s, w)| {
                if *s == socket {
                    w.wake_by_ref();
                }
                *s != socket
            });
        }
    }

    /// Wakes the caller on the next event of `socket`.
    pub fn register(&mut self, socket: SocketId, waker: &Waker) {
        if !self.waiters.iter().any(|(s, w)| *s == socket && w.will_wake(waker)) {
            self.waiters.push((socket, waker.clone()));
        }
    }

    pub fn forget(&mut self, socket: SocketId) {
        self.waiters.retain(|(s, _)| *s != socket);
    }
}

/// A stack with its links, shared by the tasks that use it.
pub(crate) struct Net {
    pub config: Config,
    inner: Mutex<Inner>,
}

static GLOBAL: OnceLock<Net> = OnceLock::new();

pub(crate) fn global() -> Result<&'static Net> {
    GLOBAL.get().ok_or(Error::NotStarted)
}

pub(crate) fn now() -> Instant {
    Instant::from_micros(idf::time::Instant::now().since_boot().as_micros() as u64)
}

/// Calls `f` under the lock until it returns `Some`, sleeping in between
/// until an event on `socket`.
pub(crate) async fn wait_for<T>(net: &Net, socket: SocketId, mut f: impl FnMut(&mut Inner, Instant) -> Option<T>) -> T {
    let waker = current_waker().await;
    loop {
        let mut inner = net.lock_async().await;
        if let Some(value) = f(&mut inner, now()) {
            return value;
        }
        inner.register(socket, &waker);
        drop(inner);
        woken().await;
    }
}

/// Resolves on the first poll after it was woken, or spuriously.
async fn woken() {
    let mut first = true;
    poll_fn(|_| {
        if first {
            first = false;
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await
}

async fn current_waker() -> Waker {
    poll_fn(|cx| Poll::Ready(cx.waker().clone())).await
}

impl Net {
    pub fn new(config: Config) -> Self {
        Self { config, inner: Mutex::new(Inner { stack: Stack::new(), links: Vec::new(), waiters: Vec::new() }) }
    }

    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock()
    }

    pub async fn lock_async(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock_async().await
    }

    pub fn add_link(&self, name: &'static str, link: Arc<dyn Link>) -> IfaceId {
        let info: LinkInfo = link.info();
        let mut inner = self.lock();
        let id = inner.stack.add_interface(name, info);
        let index = id.index();
        if inner.links.len() <= index {
            inner.links.resize_with(index + 1, || None);
        }
        inner.links[index] = Some(link);
        id
    }

    pub fn receive(&self, id: IfaceId, frame: &[u8], now: Instant) {
        self.lock().with_tx(|stack, tx| stack.receive(id, frame, now, tx));
    }

    pub fn poll(&self, now: Instant) -> Option<Instant> {
        self.lock().with_tx(|stack, tx| stack.poll(now, tx))
    }
}
