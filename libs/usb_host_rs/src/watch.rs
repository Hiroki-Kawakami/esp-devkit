/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::future::poll_fn;
use core::task::{Poll, Waker};

use idf::sync::Mutex;

use crate::Device;

#[derive(Clone)]
pub enum Event {
    Arrived(Device),
    Left(Device),
}

struct Subscriber {
    queue: Mutex<(VecDeque<Event>, Option<Waker>)>,
}

impl Subscriber {
    fn push(&self, event: Event) {
        let waker = {
            let mut q = self.queue.lock();
            q.0.push_back(event);
            q.1.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct Registry {
    devices: Vec<Device>,
    subscribers: Vec<Weak<Subscriber>>,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry { devices: Vec::new(), subscribers: Vec::new() });

/// Devices plugged in and enumerated.
pub fn devices() -> Vec<Device> {
    REGISTRY.lock().devices.clone()
}

/// Every device plugged in now, as `Arrived`, then every arrival and
/// departure after it, in order.
pub fn watch() -> Watch {
    let subscriber = Arc::new(Subscriber { queue: Mutex::new((VecDeque::new(), None)) });
    let mut registry = REGISTRY.lock();
    for device in &registry.devices {
        subscriber.push(Event::Arrived(device.clone()));
    }
    registry.subscribers.push(Arc::downgrade(&subscriber));
    Watch(subscriber)
}

/// Events queue up until read; dropping it unsubscribes.
pub struct Watch(Arc<Subscriber>);

impl Watch {
    pub async fn next(&mut self) -> Event {
        poll_fn(|cx| {
            let mut q = self.0.queue.lock();
            match q.0.pop_front() {
                Some(event) => Poll::Ready(event),
                None => {
                    q.1 = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await
    }
}

fn publish(registry: &mut Registry, event: Event) {
    registry.subscribers.retain(|s| match s.upgrade() {
        Some(subscriber) => {
            subscriber.push(event.clone());
            true
        }
        None => false,
    });
}

pub(crate) fn arrived(device: Device) {
    let mut registry = REGISTRY.lock();
    registry.devices.push(device.clone());
    publish(&mut registry, Event::Arrived(device));
}

pub(crate) fn left(device: Device) {
    let mut registry = REGISTRY.lock();
    registry.devices.retain(|d| d.id() != device.id());
    publish(&mut registry, Event::Left(device));
}
