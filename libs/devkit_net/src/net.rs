/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::{Future, pending, poll_fn};
use core::pin::pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Poll, Waker};
use core::time::Duration;

use idf::executor::{self, CRITICAL, JoinHandle, LocalSpawner, Receiver, Sender, Signal};
use idf::heap::Caps;
use idf::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use idf::thread::Builder;

use crate::dhcp;
use crate::link::{Link, LinkRx, RxFrame, RxSink};
use crate::stack::{IfaceId, Ipv4Config, SocketId, Stack, Tx};
use crate::time::Instant;
use crate::{Error, Result};

const RX_QUEUE: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Receive buffer of a UDP socket that sets none of its own.
    pub udp_rx_buffer: usize,
    pub tcp_rx_buffer: usize,
    pub tcp_tx_buffer: usize,
    /// Where socket buffers are allocated unless a socket says otherwise.
    pub caps: Caps,
    /// The stack's own task, which processes frames and timers and runs
    /// the DHCP clients.
    pub task_priority: u32,
    pub task_core: Option<u32>,
    pub task_stack: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            udp_rx_buffer: 4096,
            tcp_rx_buffer: 16384,
            tcp_tx_buffer: 16384,
            caps: Caps::DEFAULT,
            task_priority: 18,
            task_core: None,
            task_stack: 8192,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ipv4Mode {
    Off,
    Static(Ipv4Config),
    /// Leased while the link is up.
    Dhcp,
}

pub(crate) struct Inner {
    pub stack: Stack,
    links: Vec<Option<&'static dyn Link>>,
    pub modes: Vec<Ipv4Mode>,
    waiters: Vec<(SocketId, Waker)>,
    kick: &'static Signal<()>,
}

impl Inner {
    /// For calls from applications: the stack's task looks at its timers
    /// again afterwards.
    pub fn with_tx<R>(&mut self, f: impl FnOnce(&mut Stack, &mut Tx) -> R) -> R {
        let result = self.drive(f);
        self.kick.signal(());
        result
    }

    fn drive<R>(&mut self, f: impl FnOnce(&mut Stack, &mut Tx) -> R) -> R {
        let links = &self.links;
        let result = f(&mut self.stack, &mut |id, frame| {
            links.get(id.index()).copied().flatten().is_some_and(|l| l.transmit(frame).is_ok())
        });
        self.wake();
        result
    }

    pub fn wake(&mut self) {
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
    config: OnceLock<Config>,
    started: AtomicBool,
    inner: Mutex<Inner>,
    kick: &'static Signal<()>,
    rx: Sender<(IfaceId, RxFrame)>,
    rx_queue: Mutex<Option<Receiver<(IfaceId, RxFrame)>>>,
}

static GLOBAL: LazyLock<Net> = LazyLock::new(Net::new);

#[cfg(CONFIG_DEVKIT_NET_WIFI)]
static NATIVE_WIFI: AtomicBool = AtomicBool::new(false);

/// Where links are registered, started or not.
pub(crate) fn registry() -> &'static Net {
    let net = &*GLOBAL;
    #[cfg(CONFIG_DEVKIT_NET_WIFI)]
    if !NATIVE_WIFI.swap(true, Ordering::AcqRel) {
        crate::wifi_native::register();
    }
    net
}

pub(crate) fn global() -> Result<&'static Net> {
    let net = registry();
    if net.started.load(Ordering::Acquire) { Ok(net) } else { Err(Error::NotStarted) }
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

struct Sink {
    net: &'static Net,
    id: IfaceId,
}

impl RxSink for Sink {
    fn deliver(&self, frame: RxFrame) -> bool {
        self.net.started.load(Ordering::Acquire) && self.net.rx.try_send((self.id, frame)).is_ok()
    }

    fn set_up(&self, up: bool) {
        self.net.set_link_up(self.id, up);
    }
}

impl Net {
    pub fn new() -> Self {
        let kick: &'static Signal<()> = Box::leak(Box::new(Signal::new(&CRITICAL)));
        let (rx, rx_queue) = executor::channel(RX_QUEUE);
        let inner = Inner { stack: Stack::new(), links: Vec::new(), modes: Vec::new(), waiters: Vec::new(), kick };
        Self {
            config: OnceLock::new(),
            started: AtomicBool::new(false),
            inner: Mutex::new(inner),
            kick,
            rx,
            rx_queue: Mutex::new(Some(rx_queue)),
        }
    }

    /// A stack that sockets can use without `start`, driven by the caller.
    #[cfg(test)]
    pub fn with_config(config: Config) -> Self {
        let net = Self::new();
        let _ = net.config.set(config);
        net
    }

    pub fn config(&self) -> Config {
        self.config.get().copied().unwrap_or_default()
    }

    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock()
    }

    pub async fn lock_async(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock_async().await
    }

    pub fn kick(&self) {
        self.kick.signal(());
    }

    pub fn add_link(&'static self, name: &'static str, link: &'static dyn Link) -> IfaceId {
        let mut inner = self.lock();
        let id = inner.stack.add_interface(name, link.info());
        let index = id.index();
        if inner.links.len() <= index {
            inner.links.resize(index + 1, None);
            inner.modes.resize(index + 1, Ipv4Mode::Off);
        }
        inner.links[index] = Some(link);
        drop(inner);
        link.attach(LinkRx::new(Arc::new(Sink { net: self, id })));
        id
    }

    fn set_link_up(&self, id: IfaceId, up: bool) {
        let mut inner = self.lock();
        if up && let Some(link) = inner.links[id.index()] {
            inner.stack.set_link_info(id, link.info());
        }
        inner.stack.set_link_up(id, up);
        drop(inner);
        self.kick();
    }

    pub fn receive(&self, id: IfaceId, frame: &[u8], now: Instant) {
        self.lock().drive(|stack, tx| stack.receive(id, frame, now, tx));
    }

    pub fn poll(&self, now: Instant) -> Option<Instant> {
        self.lock().drive(|stack, tx| stack.poll(now, tx))
    }

    pub fn start(&'static self, config: Config) -> Result<()> {
        self.config.set(config).map_err(|_| Error::AlreadyStarted)?;
        self.started.store(true, Ordering::Release);
        Builder::new()
            .name("devkit_net")
            .priority(config.task_priority)
            .stack_size(config.task_stack)
            .core(config.task_core)
            .spawn_local_executor(move |local| {
                local.spawn_local(run(self));
            })
            .map_err(|_| Error::NoMemory)?;
        Ok(())
    }

    /// Runs a DHCP client on each interface that wants one and has its link
    /// up, and stops the others.
    fn supervise_dhcp(&'static self, spawner: &LocalSpawner, clients: &mut Vec<Option<JoinHandle<()>>>) {
        let wanted: Vec<(IfaceId, bool, bool)> = {
            let inner = self.lock();
            inner
                .stack
                .iface_ids()
                .map(|id| {
                    let dhcp = inner.modes.get(id.index()) == Some(&Ipv4Mode::Dhcp);
                    (id, dhcp, dhcp && inner.stack.is_link_up(id))
                })
                .collect()
        };
        for (id, dhcp, want) in wanted {
            if clients.len() <= id.index() {
                clients.resize_with(id.index() + 1, || None);
            }
            let slot = &mut clients[id.index()];
            if want && slot.is_none() {
                *slot = Some(spawner.spawn_local(dhcp::run(self, id)));
            } else if !want && let Some(client) = slot.take() {
                client.abort();
                if dhcp {
                    let mut inner = self.lock();
                    inner.stack.set_ipv4(id, None);
                    inner.stack.set_dns_servers(id, &[]);
                    inner.wake();
                }
            }
        }
    }
}

/// The stack's task: frames from the links, timers, and DHCP.
async fn run(net: &'static Net) {
    let mut rx = net.rx_queue.lock().take().unwrap();
    let spawner = executor::local_spawner().await;
    let mut clients = Vec::new();
    loop {
        net.supervise_dhcp(&spawner, &mut clients);
        while let Some((id, frame)) = rx.try_recv() {
            net.receive(id, &frame, now());
        }
        let next = net.poll(now());
        let wait = next.map(|at| at.saturating_duration_since(now()));
        if wait == Some(Duration::ZERO) {
            continue;
        }
        if let Some((id, frame)) = next_event(&mut rx, net.kick, wait).await {
            net.receive(id, &frame, now());
        }
    }
}

/// A frame, or `None` once kicked or after `wait`.
async fn next_event<T>(rx: &mut Receiver<T>, kick: &Signal<()>, wait: Option<Duration>) -> Option<T> {
    let mut recv = pin!(rx.recv());
    let mut kicked = pin!(kick.wait());
    let mut timer = pin!(async {
        match wait {
            Some(d) => executor::sleep(d).await,
            None => pending().await,
        }
    });
    poll_fn(|cx| {
        if let Poll::Ready(value) = recv.as_mut().poll(cx) {
            return Poll::Ready(Some(value));
        }
        if kicked.as_mut().poll(cx).is_ready() || timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}
