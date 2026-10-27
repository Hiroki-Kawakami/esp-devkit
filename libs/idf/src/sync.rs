/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::marker::PhantomData;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::time::Duration;

use crate::thread::{MAX_DELAY, ticks};
use crate::{Error, Result, sys};

const PD_TRUE: sys::BaseType_t = 1;
const QUEUE_TYPE_BASE: u8 = 0;
const QUEUE_TYPE_MUTEX: u8 = 1;
const SEND_TO_BACK: sys::BaseType_t = 0;

#[cfg(all(target_os = "none", target_arch = "riscv32"))]
unsafe extern "C" {
    fn vPortYieldFromISR();
}

#[cfg(all(target_os = "none", target_arch = "xtensa"))]
unsafe extern "C" {
    fn _frxt_setup_switch();
}

// portYIELD_FROM_ISR only marks a switch for when the ISR returns, so calling
// it mid-ISR is fine.
fn yield_from_isr(woken: sys::BaseType_t) {
    if woken == PD_TRUE {
        #[cfg(all(target_os = "none", target_arch = "riscv32"))]
        unsafe {
            vPortYieldFromISR()
        }
        #[cfg(all(target_os = "none", target_arch = "xtensa"))]
        unsafe {
            _frxt_setup_switch()
        }
    }
}

struct Queue(sys::QueueHandle_t);

unsafe impl Send for Queue {}
unsafe impl Sync for Queue {}

impl Queue {
    fn new(handle: sys::QueueHandle_t) -> Result<Self> {
        if handle.is_null() { Err(Error::NO_MEM) } else { Ok(Self(handle)) }
    }

    fn send(&self, item: *const c_void, ticks: sys::TickType_t) -> bool {
        unsafe { sys::xQueueGenericSend(self.0, item, ticks, SEND_TO_BACK) == PD_TRUE }
    }

    fn receive(&self, buf: *mut c_void, ticks: sys::TickType_t) -> bool {
        unsafe { sys::xQueueReceive(self.0, buf, ticks) == PD_TRUE }
    }

    fn take(&self, ticks: sys::TickType_t) -> bool {
        unsafe { sys::xQueueSemaphoreTake(self.0, ticks) == PD_TRUE }
    }

    fn send_from_isr(&self, item: *const c_void) -> bool {
        let mut woken = 0;
        let sent = unsafe { sys::xQueueGenericSendFromISR(self.0, item, &mut woken, SEND_TO_BACK) == PD_TRUE };
        yield_from_isr(woken);
        sent
    }

    fn receive_from_isr(&self, buf: *mut c_void) -> bool {
        let mut woken = 0;
        let received = unsafe { sys::xQueueReceiveFromISR(self.0, buf, &mut woken) == PD_TRUE };
        yield_from_isr(woken);
        received
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        unsafe { sys::vQueueDelete(self.0) }
    }
}

pub struct Mutex<T: ?Sized> {
    queue: Queue,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        match Self::try_new(value) {
            Ok(m) => m,
            Err(e) => panic!("failed to create mutex: {e}"),
        }
    }

    pub fn try_new(value: T) -> Result<Self> {
        let queue = Queue::new(unsafe { sys::xQueueCreateMutex(QUEUE_TYPE_MUTEX) })?;
        Ok(Self { queue, data: UnsafeCell::new(value) })
    }

    pub fn into_inner(self) -> T {
        let Self { queue, data } = self;
        drop(queue);
        data.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        match self.lock_ticks(MAX_DELAY) {
            Some(guard) => guard,
            None => unreachable!(),
        }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.lock_ticks(0)
    }

    pub fn lock_timeout(&self, timeout: Duration) -> Option<MutexGuard<'_, T>> {
        self.lock_ticks(ticks(timeout))
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }

    fn lock_ticks(&self, ticks: sys::TickType_t) -> Option<MutexGuard<'_, T>> {
        self.queue.take(ticks).then(|| MutexGuard { mutex: self, _not_send: PhantomData })
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// A FreeRTOS mutex must be given back by the task that took it.
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
    _not_send: PhantomData<*const ()>,
}

unsafe impl<T: ?Sized + Sync> Sync for MutexGuard<'_, T> {}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.queue.send(ptr::null(), 0);
    }
}

pub struct Semaphore {
    queue: Queue,
}

impl Semaphore {
    pub fn new(initial: u32, max: u32) -> Self {
        match Self::try_new(initial, max) {
            Ok(s) => s,
            Err(e) => panic!("failed to create semaphore: {e}"),
        }
    }

    pub fn try_new(initial: u32, max: u32) -> Result<Self> {
        if max == 0 || initial > max {
            return Err(Error::INVALID_ARG);
        }
        let handle = unsafe { sys::xQueueCreateCountingSemaphore(max as _, initial as _) };
        Ok(Self { queue: Queue::new(handle)? })
    }

    pub fn take(&self) {
        if !self.queue.take(MAX_DELAY) {
            unreachable!();
        }
    }

    pub fn try_take(&self) -> bool {
        self.queue.take(0)
    }

    pub fn take_timeout(&self, timeout: Duration) -> bool {
        self.queue.take(ticks(timeout))
    }

    pub fn give(&self) -> bool {
        self.queue.send(ptr::null(), 0)
    }

    pub fn count(&self) -> u32 {
        unsafe { sys::uxQueueMessagesWaiting(self.queue.0) as u32 }
    }

    pub fn give_from_isr(&self) -> bool {
        let mut woken = 0;
        let given = unsafe { sys::xQueueGiveFromISR(self.queue.0, &mut woken) == PD_TRUE };
        yield_from_isr(woken);
        given
    }

    pub fn try_take_from_isr(&self) -> bool {
        self.queue.receive_from_isr(ptr::null_mut())
    }
}

struct Channel<T> {
    queue: Queue,
    _items: PhantomData<T>,
}

unsafe impl<T: Send> Send for Channel<T> {}
unsafe impl<T: Send> Sync for Channel<T> {}

impl<T> Channel<T> {
    fn send(&self, value: T, ticks: sys::TickType_t) -> core::result::Result<(), T> {
        let value = ManuallyDrop::new(value);
        if self.queue.send((&*value as *const T).cast(), ticks) {
            Ok(())
        } else {
            Err(ManuallyDrop::into_inner(value))
        }
    }

    fn receive(&self, ticks: sys::TickType_t) -> Option<T> {
        let mut slot = MaybeUninit::<T>::uninit();
        self.queue.receive(slot.as_mut_ptr().cast(), ticks).then(|| unsafe { slot.assume_init() })
    }
}

impl<T> Drop for Channel<T> {
    fn drop(&mut self) {
        while self.receive(0).is_some() {}
    }
}

pub fn channel<T: Send>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    assert!(capacity > 0, "channel capacity must be at least 1");
    let handle = unsafe {
        sys::xQueueGenericCreate(capacity as _, core::mem::size_of::<T>() as _, QUEUE_TYPE_BASE)
    };
    let queue = match Queue::new(handle) {
        Ok(q) => q,
        Err(e) => panic!("failed to create channel: {e}"),
    };
    let chan = Arc::new(Channel { queue, _items: PhantomData });
    (Sender(chan.clone()), Receiver(chan))
}

pub struct Sender<T>(Arc<Channel<T>>);

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Sender<T> {
    pub fn send(&self, value: T) {
        if self.0.send(value, MAX_DELAY).is_err() {
            unreachable!();
        }
    }

    pub fn try_send(&self, value: T) -> core::result::Result<(), T> {
        self.0.send(value, 0)
    }

    pub fn send_timeout(&self, value: T, timeout: Duration) -> core::result::Result<(), T> {
        self.0.send(value, ticks(timeout))
    }

    pub fn try_send_from_isr(&self, value: T) -> core::result::Result<(), T> {
        let value = ManuallyDrop::new(value);
        if self.0.queue.send_from_isr((&*value as *const T).cast()) {
            Ok(())
        } else {
            Err(ManuallyDrop::into_inner(value))
        }
    }
}

pub struct Receiver<T>(Arc<Channel<T>>);

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> T {
        match self.0.receive(MAX_DELAY) {
            Some(value) => value,
            None => unreachable!(),
        }
    }

    pub fn try_recv(&self) -> Option<T> {
        self.0.receive(0)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<T> {
        self.0.receive(ticks(timeout))
    }

    pub fn try_recv_from_isr(&self) -> Option<T> {
        let mut slot = MaybeUninit::<T>::uninit();
        self.0.queue.receive_from_isr(slot.as_mut_ptr().cast()).then(|| unsafe { slot.assume_init() })
    }
}

#[cfg(test)]
mod tests {
    use super::{Mutex, Semaphore, channel};
    use crate::thread::{sleep, spawn};
    use crate::time::Instant;
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::time::Duration;

    #[test]
    fn mutex_excludes() {
        let m = Arc::new(Mutex::new(0u32));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let m = m.clone();
                spawn(move || {
                    for _ in 0..1000 {
                        *m.lock() += 1;
                    }
                })
            })
            .collect();
        for h in handles {
            h.join();
        }
        assert_eq!(*m.lock(), 4000);
    }

    #[test]
    fn mutex_try_and_timeout() {
        let m = Arc::new(Mutex::new(1));
        let guard = m.lock();
        let other = m.clone();
        let h = spawn(move || {
            assert!(other.try_lock().is_none());
            let start = Instant::now();
            assert!(other.lock_timeout(Duration::from_millis(20)).is_none());
            assert!(start.elapsed() >= Duration::from_millis(20));
        });
        h.join();
        drop(guard);
        assert_eq!(*m.try_lock().unwrap(), 1);
        let mut m = Arc::try_unwrap(m).ok().unwrap();
        *m.get_mut() = 5;
        assert_eq!(m.into_inner(), 5);
    }

    #[test]
    fn semaphore_counts() {
        assert!(Semaphore::try_new(2, 1).is_err());
        assert!(Semaphore::try_new(0, 0).is_err());
        let s = Semaphore::new(1, 2);
        assert_eq!(s.count(), 1);
        assert!(s.give());
        assert!(!s.give());
        assert_eq!(s.count(), 2);
        s.take();
        assert!(s.try_take());
        assert!(!s.try_take());
        assert!(!s.take_timeout(Duration::from_millis(10)));
        assert!(s.give_from_isr());
        assert!(s.try_take_from_isr());
        assert!(!s.try_take_from_isr());
    }

    #[test]
    fn semaphore_signals_another_task() {
        let s = Arc::new(Semaphore::new(0, 1));
        let other = s.clone();
        let h = spawn(move || {
            sleep(Duration::from_millis(10));
            other.give();
        });
        s.take();
        h.join();
    }

    #[test]
    fn channel_passes_values() {
        let (tx, rx) = channel::<String>(2);
        let tx2 = tx.clone();
        let h = spawn(move || {
            for i in 0..5 {
                tx2.send(format!("v{i}"));
            }
        });
        let got: Vec<String> = (0..5).map(|_| rx.recv()).collect();
        h.join();
        assert_eq!(got, ["v0", "v1", "v2", "v3", "v4"]);
        assert_eq!(rx.try_recv(), None);
        assert_eq!(rx.recv_timeout(Duration::from_millis(10)), None);
        tx.send("a".into());
        tx.send("b".into());
        assert_eq!(tx.try_send("c".into()), Err("c".into()));
        assert_eq!(tx.send_timeout("d".into(), Duration::from_millis(10)), Err("d".into()));
        assert_eq!(rx.try_recv_from_isr().as_deref(), Some("a"));
        assert_eq!(tx.try_send_from_isr("e".into()), Ok(()));
        assert_eq!(rx.recv(), "b");
        assert_eq!(rx.recv(), "e");
    }

    #[test]
    fn channel_drops_unreceived_values() {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        struct Counted;
        impl Drop for Counted {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }
        let (tx, rx) = channel::<Counted>(4);
        tx.send(Counted);
        tx.send(Counted);
        tx.send(Counted);
        drop(rx.recv());
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
        drop(tx);
        drop(rx);
        assert_eq!(DROPS.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn channel_of_zero_sized_items() {
        let (tx, rx) = channel::<()>(1);
        tx.send(());
        assert_eq!(tx.try_send(()), Err(()));
        assert_eq!(rx.recv(), ());
    }
}
