/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod sync;
mod task;
#[cfg(test)]
mod tests;
mod timer;

use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::{Pin, pin};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

pub use sync::{Receiver, SendFuture, Sender, Signal, channel};
pub use task::JoinHandle;
pub use timer::{Elapsed, Timer, sleep, sleep_until, timeout};
use task::{ABORTED, COMPLETE, HANDLE, Header, RUNNING, SCHEDULED, TaskRef};

const MAX_RUNNERS: usize = 8;

/// How a runner waits for work.
pub trait Park: Send + Sync {
    /// Returns once `unpark` was called, also before this call, or after
    /// `timeout` ticks.
    fn park(&self, timeout: Option<u32>);
    /// Callable from any context, interrupts included.
    fn unpark(&self);
    fn now(&self) -> u32;
}

/// Keeps out every context that may wake a task: other tasks, other cores and
/// interrupts. `f` neither blocks nor allocates.
pub trait CriticalSection: Sync {
    fn with(&self, f: &mut dyn FnMut());
}

struct Runner {
    park: Arc<dyn Park>,
    idle: bool,
}

struct Shared {
    head: *const Header,
    tail: *const Header,
    all: *const Header,
    timers: *mut timer::Node,
    clock: Option<Arc<dyn Park>>,
    runners: [Option<Runner>; MAX_RUNNERS],
    next_claim: usize,
    shut_down: bool,
}

pub(crate) struct Inner {
    critical: &'static dyn CriticalSection,
    local: bool,
    shared: UnsafeCell<Shared>,
}

unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Inner {
    fn new(critical: &'static dyn CriticalSection, local: bool) -> Arc<Self> {
        const NONE: Option<Runner> = None;
        Arc::new(Self {
            critical,
            local,
            shared: UnsafeCell::new(Shared {
                head: ptr::null(),
                tail: ptr::null(),
                all: ptr::null(),
                timers: ptr::null_mut(),
                clock: None,
                runners: [NONE; MAX_RUNNERS],
                next_claim: 0,
                shut_down: false,
            }),
        })
    }

    fn critical<R>(&self, f: impl FnOnce() -> R) -> R {
        let mut f = Some(f);
        let mut out = None;
        self.critical.with(&mut || out = Some((f.take().unwrap())()));
        out.unwrap()
    }

    /// Inside the critical section.
    #[allow(clippy::mut_from_ref)]
    unsafe fn shared(&self) -> &mut Shared {
        unsafe { &mut *self.shared.get() }
    }

    /// Inside the critical section. An idle runner to unpark, now marked busy.
    // Taking them in turn: always the first would hand every task to a runner
    // that preempts the spawner and runs it before the next spawn, leaving the
    // other cores idle.
    unsafe fn claim_idle(&self) -> Option<Arc<dyn Park>> {
        let shared = unsafe { self.shared() };
        let start = shared.next_claim;
        let slot = (0..MAX_RUNNERS)
            .map(|i| (start + i) % MAX_RUNNERS)
            .find(|&i| shared.runners[i].as_ref().is_some_and(|r| r.idle))?;
        shared.next_claim = (slot + 1) % MAX_RUNNERS;
        let runner = shared.runners[slot].as_mut().unwrap();
        runner.idle = false;
        Some(runner.park.clone())
    }

    fn spawn<F: Future + 'static>(self: &Arc<Self>, future: F) -> JoinHandle<F::Output> {
        let task = TaskRef::new(future, self.clone(), SCHEDULED | HANDLE);
        let handle = JoinHandle::new(task.clone());
        let registry = task.clone();
        let mut unpark = None;
        let mut rejected = None;
        self.critical(|| {
            let shared = unsafe { self.shared() };
            if shared.shut_down {
                rejected = Some((registry, task));
                return;
            }
            let ptr = registry.into_raw();
            unsafe {
                *(*ptr).all_next.get() = shared.all;
                if !shared.all.is_null() {
                    *(*shared.all).all_prev.get() = ptr;
                }
            }
            shared.all = ptr;
            unsafe { self.push(task) };
            unpark = unsafe { self.claim_idle() };
        });
        if let Some((_registry, task)) = rejected {
            task.header().state.fetch_or(COMPLETE, Ordering::Release);
            unsafe { task.drop_stage() };
        }
        if let Some(park) = unpark {
            park.unpark();
        }
        handle
    }

    /// Inside the critical section; the queue keeps the reference.
    unsafe fn push(&self, task: TaskRef) {
        let shared = unsafe { self.shared() };
        let ptr = task.into_raw();
        unsafe { *(*ptr).queue_next.get() = ptr::null() };
        if shared.tail.is_null() {
            shared.head = ptr;
        } else {
            unsafe { *(*shared.tail).queue_next.get() = ptr };
        }
        shared.tail = ptr;
    }

    /// Inside the critical section.
    unsafe fn pop(&self) -> Option<TaskRef> {
        let shared = unsafe { self.shared() };
        if shared.head.is_null() {
            return None;
        }
        let ptr = shared.head;
        shared.head = unsafe { *(*ptr).queue_next.get() };
        if shared.head.is_null() {
            shared.tail = ptr::null();
        }
        Some(unsafe { TaskRef::from_raw(ptr) })
    }

    /// Inside the critical section. The registry's reference to the task.
    unsafe fn unregister(&self, ptr: *const Header) -> TaskRef {
        let shared = unsafe { self.shared() };
        unsafe {
            let prev = *(*ptr).all_prev.get();
            let next = *(*ptr).all_next.get();
            if prev.is_null() {
                shared.all = next;
            } else {
                *(*prev).all_next.get() = next;
            }
            if !next.is_null() {
                *(*next).all_prev.get() = prev;
            }
            TaskRef::from_raw(ptr)
        }
    }

    fn register_runner(&self, park: Arc<dyn Park>) -> usize {
        self.critical(|| {
            let shared = unsafe { self.shared() };
            let slot = shared.runners.iter().position(Option::is_none).expect("too many runners on one executor");
            if shared.clock.is_none() {
                shared.clock = Some(park.clone());
            }
            shared.runners[slot] = Some(Runner { park, idle: false });
            slot
        })
    }

    /// A task off the queue, or `None` with the runner marked idle.
    fn next(&self, runner: usize) -> Option<TaskRef> {
        self.critical(|| unsafe {
            let task = self.pop();
            if task.is_none() {
                self.shared().runners[runner].as_mut().unwrap().idle = true;
            }
            task
        })
    }

    /// Runs one task, or waits for one; `main` set means a future outside
    /// the queue wants polling.
    fn turn(&self, runner: usize, park: &dyn Park, main: Option<&AtomicBool>) {
        if let Some(task) = self.next(runner) {
            let now = park.now();
            self.fire_timers(now);
            self.run_task(task);
            return;
        }
        let wait = self.fire_timers(park.now());
        if main.is_none_or(|woken| !woken.load(Ordering::Acquire)) {
            park.park(wait);
        }
        self.set_busy(runner);
    }

    fn set_busy(&self, runner: usize) {
        self.critical(|| unsafe { self.shared().runners[runner].as_mut().unwrap().idle = false });
    }

    fn run_task(&self, task: TaskRef) {
        let header = task.header();
        let prev = header.state.fetch_update(Ordering::AcqRel, Ordering::Acquire, |s| {
            Some((s & !SCHEDULED) | RUNNING)
        });
        let prev = prev.unwrap();
        if prev & COMPLETE != 0 {
            return;
        }
        let finished = if prev & ABORTED != 0 {
            unsafe { task.drop_stage() };
            true
        } else {
            let waker = task.waker();
            unsafe { task.poll(&mut Context::from_waker(&waker)) }
        };
        if finished {
            self.finish(&task);
            return;
        }
        let prev = header.state.fetch_and(!RUNNING, Ordering::AcqRel);
        if prev & SCHEDULED != 0 {
            enqueue(task);
        }
    }

    fn finish(&self, task: &TaskRef) {
        let header = task.header();
        let mut handle = false;
        let mut waker = None;
        let mut registry = None;
        self.critical(|| {
            let prev = header.state.fetch_update(Ordering::AcqRel, Ordering::Acquire, |s| {
                Some((s & !(RUNNING | SCHEDULED)) | COMPLETE)
            });
            handle = prev.unwrap() & HANDLE != 0;
            waker = unsafe { (*header.join_waker.get()).take() };
            registry = Some(unsafe { self.unregister(task.as_ptr()) });
        });
        if !handle {
            unsafe { task.drop_stage() };
        }
        if let Some(waker) = waker {
            waker.wake();
        }
        drop(registry);
    }

    /// Drops every task still here, on the calling task.
    fn shut_down(&self) {
        let (queue, all) = self.critical(|| unsafe {
            let shared = self.shared();
            shared.shut_down = true;
            let queue = shared.head;
            shared.head = ptr::null();
            shared.tail = ptr::null();
            let all = shared.all;
            shared.all = ptr::null();
            (queue, all)
        });
        let mut ptr = queue;
        while !ptr.is_null() {
            let task = unsafe { TaskRef::from_raw(ptr) };
            ptr = unsafe { *(*ptr).queue_next.get() };
            drop(task);
        }
        let mut ptr = all;
        while !ptr.is_null() {
            let task = unsafe { TaskRef::from_raw(ptr) };
            ptr = unsafe { *(*ptr).all_next.get() };
            let header = task.header();
            let mut waker = None;
            self.critical(|| {
                header.state.fetch_or(COMPLETE, Ordering::AcqRel);
                waker = unsafe { (*header.join_waker.get()).take() };
            });
            unsafe { task.drop_stage() };
            drop(waker);
        }
    }
}

/// Queues a task woken outside its run.
fn enqueue(task: TaskRef) {
    let executor = task.header().executor.clone();
    let mut unpark = None;
    let mut dropped = None;
    executor.critical(|| unsafe {
        if executor.shared().shut_down {
            dropped = Some(task);
            return;
        }
        executor.push(task);
        unpark = executor.claim_idle();
    });
    drop(dropped);
    if let Some(park) = unpark {
        park.unpark();
    }
}

fn wake(task: TaskRef) {
    let prev = task.header().state.fetch_or(SCHEDULED, Ordering::AcqRel);
    if prev & (SCHEDULED | RUNNING | COMPLETE) == 0 {
        enqueue(task);
    }
}

struct Main {
    woken: AtomicBool,
    executor: Arc<Inner>,
    park: Arc<dyn Park>,
}

static MAIN_VTABLE: RawWakerVTable = RawWakerVTable::new(
    |data| {
        let main = unsafe { Arc::from_raw(data.cast::<Main>()) };
        let clone = Arc::into_raw(main.clone());
        core::mem::forget(main);
        RawWaker::new(clone.cast(), &MAIN_VTABLE)
    },
    |data| {
        let main = unsafe { Arc::from_raw(data.cast::<Main>()) };
        main.woken.store(true, Ordering::Release);
        main.park.unpark();
    },
    |data| {
        let main = unsafe { &*data.cast::<Main>() };
        main.woken.store(true, Ordering::Release);
        main.park.unpark();
    },
    |data| drop(unsafe { Arc::from_raw(data.cast::<Main>()) }),
);

/// Runs on one task; its futures need not be `Send`.
pub struct LocalExecutor {
    inner: Arc<Inner>,
    park: Arc<dyn Park>,
    _local: PhantomData<*const ()>,
}

impl LocalExecutor {
    /// `park` waits on the task that will call `run`.
    pub fn new(critical: &'static dyn CriticalSection, park: Arc<dyn Park>) -> Self {
        Self { inner: Inner::new(critical, true), park, _local: PhantomData }
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send,
    {
        self.inner.spawn(future)
    }

    pub fn spawn_local<F: Future + 'static>(&self, future: F) -> JoinHandle<F::Output> {
        self.inner.spawn(future)
    }

    pub fn spawner(&self) -> Spawner {
        Spawner { inner: self.inner.clone() }
    }

    pub fn run(&self) -> ! {
        let runner = self.inner.register_runner(self.park.clone());
        loop {
            self.inner.turn(runner, &*self.park, None);
        }
    }

    /// Runs the executor's tasks until `future`, polled on this task, is done.
    pub fn run_until<F: Future>(&self, future: F) -> F::Output {
        let runner = self.inner.register_runner(self.park.clone());
        let main = Arc::new(Main { woken: AtomicBool::new(true), executor: self.inner.clone(), park: self.park.clone() });
        let waker = unsafe { Waker::from_raw(RawWaker::new(Arc::into_raw(main.clone()).cast(), &MAIN_VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut future = pin!(future);
        let output = loop {
            if main.woken.swap(false, Ordering::AcqRel)
                && let Poll::Ready(output) = future.as_mut().poll(&mut cx)
            {
                break output;
            }
            self.inner.turn(runner, &*self.park, Some(&main.woken));
        };
        self.inner.critical(|| unsafe { self.inner.shared().runners[runner] = None });
        output
    }
}

impl Drop for LocalExecutor {
    fn drop(&mut self) {
        self.inner.shut_down();
    }
}

/// Runs on every task that calls `run`, one per core typically.
pub struct Executor {
    inner: Arc<Inner>,
}

impl Executor {
    pub fn new(critical: &'static dyn CriticalSection) -> Self {
        Self { inner: Inner::new(critical, false) }
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send,
    {
        self.inner.spawn(future)
    }

    pub fn spawner(&self) -> Spawner {
        Spawner { inner: self.inner.clone() }
    }

    /// `park` waits on the calling task.
    pub fn run(&self, park: Arc<dyn Park>) -> ! {
        let runner = self.inner.register_runner(park.clone());
        loop {
            self.inner.turn(runner, &*park, None);
        }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.inner.shut_down();
    }
}

/// Spawns onto an executor from anywhere.
#[derive(Clone)]
pub struct Spawner {
    inner: Arc<Inner>,
}

impl Spawner {
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send,
    {
        self.inner.spawn(future)
    }
}

/// Spawns futures that need not be `Send` onto the `LocalExecutor` it came from.
pub struct LocalSpawner {
    inner: Arc<Inner>,
    _local: PhantomData<*const ()>,
}

impl LocalSpawner {
    pub fn spawn_local<F: Future + 'static>(&self, future: F) -> JoinHandle<F::Output> {
        self.inner.spawn(future)
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send,
    {
        self.inner.spawn(future)
    }
}

fn current(cx: &Context<'_>) -> Option<Arc<Inner>> {
    let waker = cx.waker();
    if let Some(header) = task::task_of(waker) {
        return Some(header.executor.clone());
    }
    (waker.vtable() == &MAIN_VTABLE).then(|| unsafe { &*waker.data().cast::<Main>() }.executor.clone())
}

struct Current;

impl Future for Current {
    type Output = Arc<Inner>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Arc<Inner>> {
        Poll::Ready(current(cx).expect("not on a devkit executor"))
    }
}

/// The executor running the caller. Panics outside one.
pub async fn spawner() -> Spawner {
    Spawner { inner: Current.await }
}

/// The `LocalExecutor` running the caller. Panics outside one.
pub async fn local_spawner() -> LocalSpawner {
    let inner = Current.await;
    assert!(inner.local, "not on a LocalExecutor");
    LocalSpawner { inner, _local: PhantomData }
}

/// Runs `future` to completion on the calling task, with a `LocalExecutor`
/// for whatever it spawns; those tasks are dropped when it returns.
pub fn block_on<F: Future>(critical: &'static dyn CriticalSection, park: Arc<dyn Park>, future: F) -> F::Output {
    LocalExecutor::new(critical, park).run_until(future)
}
