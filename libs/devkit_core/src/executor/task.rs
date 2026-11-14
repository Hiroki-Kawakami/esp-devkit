/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::future::Future;
use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::pin::Pin;
use core::ptr;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use super::Inner;

pub(super) const SCHEDULED: u32 = 1 << 0;
pub(super) const RUNNING: u32 = 1 << 1;
pub(super) const COMPLETE: u32 = 1 << 2;
pub(super) const ABORTED: u32 = 1 << 3;
pub(super) const HANDLE: u32 = 1 << 4;

pub(super) struct VTable {
    /// true once the future finished; the output is then stored.
    poll: unsafe fn(*const Header, &mut Context<'_>) -> bool,
    drop_stage: unsafe fn(*const Header),
    /// Moves the output to `out`, a `*mut Option<T>`.
    take_output: unsafe fn(*const Header, *mut ()),
    dealloc: unsafe fn(*const Header),
}

/// The links and the join waker belong to the executor's critical section.
#[repr(C)]
pub(super) struct Header {
    pub state: AtomicU32,
    refs: AtomicUsize,
    vtable: &'static VTable,
    pub executor: Arc<Inner>,
    pub queue_next: UnsafeCell<*const Header>,
    pub all_prev: UnsafeCell<*const Header>,
    pub all_next: UnsafeCell<*const Header>,
    pub join_waker: UnsafeCell<Option<Waker>>,
}

unsafe impl Send for Header {}
unsafe impl Sync for Header {}

enum Stage<F: Future> {
    Running(F),
    Finished(F::Output),
    Consumed,
}

#[repr(C)]
struct Task<F: Future> {
    header: Header,
    stage: UnsafeCell<Stage<F>>,
}

pub(super) struct TaskRef(*const Header);

unsafe impl Send for TaskRef {}

impl TaskRef {
    pub fn new<F: Future + 'static>(future: F, executor: Arc<Inner>, state: u32) -> Self {
        let task = Box::new(Task {
            header: Header {
                state: AtomicU32::new(state),
                refs: AtomicUsize::new(1),
                vtable: &VTableOf::<F>::VTABLE,
                executor,
                queue_next: UnsafeCell::new(ptr::null()),
                all_prev: UnsafeCell::new(ptr::null()),
                all_next: UnsafeCell::new(ptr::null()),
                join_waker: UnsafeCell::new(None),
            },
            stage: UnsafeCell::new(Stage::Running(future)),
        });
        Self(Box::into_raw(task).cast())
    }

    /// Adopts a reference given up with `into_raw`.
    pub unsafe fn from_raw(ptr: *const Header) -> Self {
        Self(ptr)
    }

    pub fn into_raw(self) -> *const Header {
        ManuallyDrop::new(self).0
    }

    pub fn as_ptr(&self) -> *const Header {
        self.0
    }

    pub fn header(&self) -> &Header {
        unsafe { &*self.0 }
    }

    /// Runs on the executor that owns the future.
    pub unsafe fn poll(&self, cx: &mut Context<'_>) -> bool {
        unsafe { (self.header().vtable.poll)(self.0, cx) }
    }

    /// Drops what is left of the future or its output; on the owning executor.
    pub unsafe fn drop_stage(&self) {
        unsafe { (self.header().vtable.drop_stage)(self.0) }
    }

    pub fn waker(&self) -> Waker {
        unsafe { Waker::from_raw(RawWaker::new(acquire(self.0).cast(), &WAKER_VTABLE)) }
    }
}

impl Clone for TaskRef {
    fn clone(&self) -> Self {
        Self(acquire(self.0))
    }
}

impl Drop for TaskRef {
    fn drop(&mut self) {
        unsafe { release(self.0) };
    }
}

fn acquire(ptr: *const Header) -> *const Header {
    unsafe { &*ptr }.refs.fetch_add(1, Ordering::Relaxed);
    ptr
}

unsafe fn release(ptr: *const Header) {
    let header = unsafe { &*ptr };
    if header.refs.fetch_sub(1, Ordering::Release) == 1 {
        core::sync::atomic::fence(Ordering::Acquire);
        unsafe { (header.vtable.dealloc)(ptr) };
    }
}

struct VTableOf<F>(PhantomData<F>);

impl<F: Future + 'static> VTableOf<F> {
    const VTABLE: VTable = VTable {
        poll: Self::poll,
        drop_stage: Self::drop_stage,
        take_output: Self::take_output,
        dealloc: Self::dealloc,
    };

    unsafe fn stage(ptr: *const Header) -> *mut Stage<F> {
        unsafe { (*ptr.cast::<Task<F>>()).stage.get() }
    }

    unsafe fn poll(ptr: *const Header, cx: &mut Context<'_>) -> bool {
        let stage = unsafe { &mut *Self::stage(ptr) };
        let Stage::Running(future) = stage else { return true };
        match unsafe { Pin::new_unchecked(future) }.poll(cx) {
            Poll::Ready(output) => {
                *stage = Stage::Finished(output);
                true
            }
            Poll::Pending => false,
        }
    }

    unsafe fn drop_stage(ptr: *const Header) {
        unsafe { *Self::stage(ptr) = Stage::Consumed };
    }

    unsafe fn take_output(ptr: *const Header, out: *mut ()) {
        let stage = unsafe { &mut *Self::stage(ptr) };
        match core::mem::replace(stage, Stage::Consumed) {
            Stage::Finished(output) => unsafe { *out.cast::<Option<F::Output>>() = Some(output) },
            _ => panic!("task output gone: its executor was dropped"),
        }
    }

    unsafe fn dealloc(ptr: *const Header) {
        drop(unsafe { Box::from_raw(ptr.cast::<Task<F>>().cast_mut()) });
    }
}

pub(super) static WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
    |data| RawWaker::new(acquire(data.cast()).cast(), &WAKER_VTABLE),
    |data| super::wake(unsafe { TaskRef::from_raw(data.cast()) }),
    |data| super::wake(TaskRef(acquire(data.cast()))),
    |data| unsafe { release(data.cast()) },
);

pub(super) fn task_of(waker: &Waker) -> Option<&Header> {
    (waker.vtable() == &WAKER_VTABLE).then(|| unsafe { &*waker.data().cast::<Header>() })
}

/// Waits for a spawned task's output. Dropping it lets the task run on
/// without anyone waiting.
pub struct JoinHandle<T> {
    task: Option<TaskRef>,
    _output: PhantomData<T>,
}

impl<T> JoinHandle<T> {
    pub(super) fn new(task: TaskRef) -> Self {
        Self { task: Some(task), _output: PhantomData }
    }

    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(|t| t.header().state.load(Ordering::Acquire) & COMPLETE != 0)
    }

    /// The future is dropped at its next suspension point, on its executor.
    pub fn abort(mut self) {
        let Some(task) = self.task.take() else { return };
        let header = task.header();
        let prev = header.state.fetch_or(ABORTED | SCHEDULED, Ordering::AcqRel);
        if prev & (SCHEDULED | RUNNING | COMPLETE) == 0 {
            super::enqueue(task.clone());
        }
        detach(task);
    }
}

fn detach(task: TaskRef) {
    let header = task.header();
    let mut drop_output = false;
    let mut waker = None;
    header.executor.critical(|| {
        let prev = header.state.fetch_and(!HANDLE, Ordering::AcqRel);
        drop_output = prev & COMPLETE != 0;
        waker = unsafe { (*header.join_waker.get()).take() };
    });
    drop(waker);
    if drop_output {
        unsafe { task.drop_stage() };
    }
}

impl<T> Unpin for JoinHandle<T> {}

impl<T> Future for JoinHandle<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let task = self.task.as_ref().expect("JoinHandle polled after completion");
        let header = task.header();
        if header.state.load(Ordering::Acquire) & COMPLETE == 0 {
            let mut waker = Some(cx.waker().clone());
            let mut complete = false;
            header.executor.critical(|| {
                complete = header.state.load(Ordering::Acquire) & COMPLETE != 0;
                if !complete {
                    waker = unsafe { (*header.join_waker.get()).replace(waker.take().unwrap()) };
                }
            });
            drop(waker);
            if !complete {
                return Poll::Pending;
            }
        }
        let mut out: Option<T> = None;
        unsafe { (header.vtable.take_output)(task.as_ptr(), (&raw mut out).cast()) };
        let task = self.task.take().unwrap();
        detach(task);
        Poll::Ready(out.unwrap())
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            detach(task);
        }
    }
}
