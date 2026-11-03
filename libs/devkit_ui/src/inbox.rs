/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::collections::VecDeque;
use alloc::collections::vec_deque::Drain;
use alloc::sync::{Arc, Weak};

use idf::sync::Mutex;

use crate::app::{Link, post};
use crate::erased::Erased;

struct Queue<T> {
    values: VecDeque<T>,
    notified: bool,
}

struct Shared<T> {
    queue: Mutex<Queue<T>>,
    capacity: usize,
    link: Link,
    id: u32,
    msg: Erased,
}

/// Values from other tasks for one component. Its message comes once for all
/// that arrive until `drain`; nothing more comes until then.
pub struct Inbox<T> {
    shared: Arc<Shared<T>>,
    drained: VecDeque<T>,
}

impl<T: Send + 'static> Inbox<T> {
    pub(crate) fn new(capacity: usize, link: Link, id: u32, msg: Erased) -> Self {
        let queue = Queue { values: VecDeque::with_capacity(capacity), notified: false };
        let shared = Arc::new(Shared { queue: Mutex::new(queue), capacity, link, id, msg });
        Self { shared, drained: VecDeque::with_capacity(capacity) }
    }

    pub fn sender(&self) -> Sender<T> {
        Sender(Arc::downgrade(&self.shared))
    }

    /// What arrived, oldest first.
    pub fn drain(&mut self) -> Drain<'_, T> {
        let mut queue = self.shared.queue.lock();
        self.drained.extend(queue.values.drain(..));
        queue.notified = false;
        drop(queue);
        self.drained.drain(..)
    }
}

/// Sends to an `Inbox` from any task.
pub struct Sender<T>(Weak<Shared<T>>);

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Sender<T> {
    /// `Err` with the value when the inbox is full or gone.
    pub fn send(&self, value: T) -> Result<(), T> {
        let Some(shared) = self.0.upgrade() else { return Err(value) };
        let mut queue = shared.queue.lock();
        if queue.values.len() >= shared.capacity {
            return Err(value);
        }
        queue.values.push_back(value);
        let notify = !core::mem::replace(&mut queue.notified, true);
        drop(queue);
        // A message the UI queue had no room for comes with the next value.
        if notify && !post(&shared.link, shared.id, shared.msg) {
            shared.queue.lock().notified = false;
        }
        Ok(())
    }
}

struct OneShot<T> {
    value: Mutex<Option<T>>,
    link: Link,
    id: u32,
    msg: Erased,
}

/// The value one `Reply` sends. Dropping it cancels the reply.
pub struct Pending<T>(Arc<OneShot<T>>);

impl<T> Pending<T> {
    pub(crate) fn new(link: Link, id: u32, msg: Erased) -> (Self, Reply<T>) {
        let shared = Arc::new(OneShot { value: Mutex::new(None), link, id, msg });
        let reply = Reply(To::Pending(Arc::downgrade(&shared)));
        (Self(shared), reply)
    }

    pub fn take(&mut self) -> Option<T> {
        self.0.value.lock().take()
    }
}

pub(crate) struct Token;

enum To<T> {
    Pending(Weak<OneShot<T>>),
    Task(Weak<Token>, Sender<T>),
}

/// Where a result goes, from any task.
pub struct Reply<T>(To<T>);

impl<T> Reply<T> {
    pub(crate) fn task(token: Weak<Token>, sender: Sender<T>) -> Self {
        Self(To::Task(token, sender))
    }

    /// Nothing waits for the result any more.
    pub fn is_cancelled(&self) -> bool {
        match &self.0 {
            To::Pending(shared) => shared.strong_count() == 0,
            To::Task(token, _) => token.strong_count() == 0,
        }
    }

    /// `Err` with the value when cancelled or it cannot be delivered.
    pub fn send(self, value: T) -> Result<(), T> {
        match self.0 {
            To::Pending(shared) => {
                let Some(shared) = shared.upgrade() else { return Err(value) };
                *shared.value.lock() = Some(value);
                if post(&shared.link, shared.id, shared.msg) {
                    return Ok(());
                }
                Err(shared.value.lock().take().expect("just stored"))
            }
            To::Task(token, sender) => {
                if token.strong_count() == 0 {
                    return Err(value);
                }
                sender.send(value)
            }
        }
    }
}
