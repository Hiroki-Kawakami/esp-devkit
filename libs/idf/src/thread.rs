/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::ffi::{CStr, c_void};
use core::marker::PhantomData;
use core::ptr;
use core::time::Duration;

use crate::heap::Caps;
use crate::sync::Semaphore;
use crate::{Error, Result, sys};

pub(crate) const MAX_DELAY: sys::TickType_t = sys::TickType_t::MAX;
const PD_PASS: sys::BaseType_t = 1;
const NO_AFFINITY: sys::BaseType_t = 0x7FFF_FFFF;
// configMAX_TASK_NAME_LEN, terminator included.
const NAME_LEN: usize = 16;

// Rounds up and adds a tick: vTaskDelay(n) ends at the n-th tick boundary, which
// may come up to a tick early.
pub(crate) fn ticks(d: Duration) -> sys::TickType_t {
    if d.is_zero() {
        return 0;
    }
    let hz = sdkconfig::CONFIG_FREERTOS_HZ as u64;
    let whole = d.as_secs().saturating_mul(hz);
    let frac = (u64::from(d.subsec_nanos()) * hz).div_ceil(1_000_000_000);
    let t = whole.saturating_add(frac).saturating_add(1);
    t.min(u64::from(MAX_DELAY)) as sys::TickType_t
}

// Rounds up only: a wait of this many ticks may end up to a tick before `d`.
pub(crate) fn ticks_ceil(d: Duration) -> sys::TickType_t {
    let hz = sdkconfig::CONFIG_FREERTOS_HZ as u64;
    let whole = d.as_secs().saturating_mul(hz);
    let frac = (u64::from(d.subsec_nanos()) * hz).div_ceil(1_000_000_000);
    whole.saturating_add(frac).min(u64::from(MAX_DELAY - 1)) as sys::TickType_t
}

pub fn sleep(d: Duration) {
    unsafe { sys::vTaskDelay(ticks(d)) }
}

pub fn yield_now() {
    unsafe { sys::vTaskDelay(0) }
}

pub fn current() -> Thread {
    Thread { handle: unsafe { sys::xTaskGetCurrentTaskHandle() }, _not_send: PhantomData }
}

// Only the task itself holds one, so the handle cannot outlive the task.
pub struct Thread {
    handle: sys::TaskHandle_t,
    _not_send: PhantomData<*const ()>,
}

impl Thread {
    pub fn name(&self) -> &str {
        let name = unsafe { CStr::from_ptr(sys::pcTaskGetName(self.handle)) };
        name.to_str().unwrap_or("")
    }

    pub fn priority(&self) -> u32 {
        unsafe { sys::uxTaskPriorityGet(self.handle) as u32 }
    }

    pub fn set_priority(&self, priority: u32) {
        unsafe { sys::vTaskPrioritySet(self.handle, priority as _) }
    }

    pub fn core_affinity(&self) -> Option<u32> {
        let core = unsafe { sys::xTaskGetCoreID(self.handle) };
        (core != NO_AFFINITY).then_some(core as u32)
    }

    pub fn stack_high_water_mark(&self) -> usize {
        unsafe { sys::uxTaskGetStackHighWaterMark(self.handle) as usize }
    }
}

pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match Builder::new().spawn(f) {
        Ok(handle) => handle,
        Err(e) => panic!("failed to spawn thread: {e}"),
    }
}

pub struct Builder {
    name: [u8; NAME_LEN],
    stack_size: usize,
    priority: u32,
    core: Option<u32>,
    stack_caps: Option<Caps>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Self {
        let mut name = [0; NAME_LEN];
        name[..4].copy_from_slice(b"task");
        Self { name, stack_size: 8192, priority: 5, core: None, stack_caps: None }
    }

    pub fn name(mut self, name: &str) -> Self {
        let name = name.split('\0').next().unwrap_or("");
        let mut len = name.len().min(NAME_LEN - 1);
        while !name.is_char_boundary(len) {
            len -= 1;
        }
        self.name = [0; NAME_LEN];
        self.name[..len].copy_from_slice(&name.as_bytes()[..len]);
        self
    }

    pub fn stack_size(mut self, bytes: usize) -> Self {
        self.stack_size = bytes;
        self
    }

    pub fn priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }

    pub fn core(mut self, core: Option<u32>) -> Self {
        self.core = core;
        self
    }

    pub fn stack_caps(mut self, caps: Caps) -> Self {
        self.stack_caps = Some(caps);
        self
    }

    pub fn spawn<F, T>(self, f: F) -> Result<JoinHandle<T>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let packet = Arc::new(Packet { result: UnsafeCell::new(None), done: Semaphore::try_new(0, 1)? });
        let with_caps = self.stack_caps.is_some();
        let start = Box::into_raw(Box::new(Start { f, packet: packet.clone(), with_caps })).cast::<c_void>();
        let name = self.name.as_ptr().cast();
        let stack = self.stack_size as _;
        let priority = self.priority as _;
        let core = self.core.map_or(NO_AFFINITY, |c| c as sys::BaseType_t);
        let entry = Some(entry::<F, T> as unsafe extern "C" fn(*mut c_void));
        let rc = unsafe {
            match self.stack_caps {
                None => sys::xTaskCreatePinnedToCore(entry, name, stack, start, priority, ptr::null_mut(), core),
                Some(caps) => sys::xTaskCreatePinnedToCoreWithCaps(
                    entry, name, stack, start, priority, ptr::null_mut(), core, caps.bits() as _),
            }
        };
        if rc != PD_PASS {
            drop(unsafe { Box::from_raw(start.cast::<Start<F, T>>()) });
            return Err(Error::NO_MEM);
        }
        Ok(JoinHandle { packet })
    }
}

struct Packet<T> {
    result: UnsafeCell<Option<T>>,
    done: Semaphore,
}

unsafe impl<T: Send> Send for Packet<T> {}
unsafe impl<T: Send> Sync for Packet<T> {}

struct Start<F, T> {
    f: F,
    packet: Arc<Packet<T>>,
    with_caps: bool,
}

unsafe extern "C" fn entry<F, T>(arg: *mut c_void)
where
    F: FnOnce() -> T,
{
    let with_caps = run::<F, T>(arg);
    unsafe {
        if with_caps {
            sys::vTaskDeleteWithCaps(ptr::null_mut());
        } else {
            sys::vTaskDelete(ptr::null_mut());
        }
    }
}

// Everything owned is dropped before the task deletes itself.
fn run<F, T>(arg: *mut c_void) -> bool
where
    F: FnOnce() -> T,
{
    let Start { f, packet, with_caps } = *unsafe { Box::from_raw(arg.cast::<Start<F, T>>()) };
    let value = f();
    unsafe { *packet.result.get() = Some(value) };
    packet.done.give();
    with_caps
}

pub struct JoinHandle<T> {
    packet: Arc<Packet<T>>,
}

impl<T> JoinHandle<T> {
    pub fn join(self) -> T {
        self.packet.done.take();
        match unsafe { (*self.packet.result.get()).take() } {
            Some(value) => value,
            None => unreachable!(),
        }
    }

    pub fn is_finished(&self) -> bool {
        Arc::strong_count(&self.packet) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::{Builder, NAME_LEN, current, sleep, spawn, ticks, ticks_ceil};
    use crate::system::{CORE_COUNT, core_id};
    use crate::heap::Caps;
    use crate::time::Instant;
    use core::time::Duration;

    #[test]
    fn ticks_round_up_and_add_one() {
        let hz = sdkconfig::CONFIG_FREERTOS_HZ as u32;
        let tick = Duration::from_secs(1) / hz;
        assert_eq!(ticks(Duration::ZERO), 0);
        assert_eq!(ticks(Duration::from_nanos(1)), 2);
        assert_eq!(ticks(tick), 2);
        assert_eq!(ticks(tick + Duration::from_nanos(1)), 3);
        assert_eq!(ticks(Duration::from_secs(1)), hz + 1);
        assert_eq!(ticks(Duration::MAX), u32::MAX);
    }

    #[test]
    fn ticks_ceil_rounds_up_only() {
        let hz = sdkconfig::CONFIG_FREERTOS_HZ as u32;
        let tick = Duration::from_secs(1) / hz;
        assert_eq!(ticks_ceil(Duration::ZERO), 0);
        assert_eq!(ticks_ceil(Duration::from_nanos(1)), 1);
        assert_eq!(ticks_ceil(tick), 1);
        assert_eq!(ticks_ceil(tick + Duration::from_nanos(1)), 2);
        assert_eq!(ticks_ceil(Duration::from_secs(1)), hz);
    }

    #[test]
    fn spawn_and_join() {
        let h = spawn(|| 6 * 7);
        assert_eq!(h.join(), 42);
        let h = Builder::new()
            .name("a-very-long-task-name")
            .stack_size(16 * 1024)
            .priority(3)
            .core(Some(0))
            .stack_caps(Caps::INTERNAL)
            .spawn(|| String::from("done"))
            .unwrap();
        assert_eq!(h.join(), "done");
    }

    #[test]
    fn name_is_truncated_on_a_char_boundary() {
        let b = Builder::new().name("ああああああ");
        assert_eq!(&b.name[..15], "あああああ".as_bytes());
        assert_eq!(b.name[15], 0);
        let b = Builder::new().name("ab\0cd");
        assert_eq!(&b.name[..3], b"ab\0");
        assert_eq!(b.name.len(), NAME_LEN);
    }

    #[test]
    fn is_finished_after_the_task_ends() {
        let h = spawn(|| sleep(Duration::from_millis(30)));
        assert!(!h.is_finished());
        while !h.is_finished() {
            sleep(Duration::from_millis(5));
        }
        h.join();
    }

    #[test]
    fn current_task_attributes() {
        let h = Builder::new()
            .name("probe")
            .stack_size(16 * 1024)
            .priority(3)
            .core(Some(1))
            .spawn(|| {
                let me = current();
                let before = me.priority();
                me.set_priority(4);
                (String::from(me.name()), before, me.priority(), me.core_affinity(),
                 me.stack_high_water_mark())
            })
            .unwrap();
        let (name, before, after, core, stack) = h.join();
        assert_eq!(name, "probe");
        assert_eq!((before, after), (3, 4));
        assert_eq!(core, Some(1));
        assert!(stack > 0);
        assert_eq!(spawn(|| current().core_affinity()).join(), None);
    }

    #[test]
    fn core_id_is_in_range() {
        assert!(CORE_COUNT >= 1);
        assert!(core_id() < CORE_COUNT);
    }

    #[test]
    fn sleep_waits_at_least_the_duration() {
        let start = Instant::now();
        sleep(Duration::from_millis(15));
        assert!(start.elapsed() >= Duration::from_millis(15));
    }
}
