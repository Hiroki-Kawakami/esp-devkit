/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::future::Future;
use core::time::Duration;

use devkit::executor::{CriticalSection, Park};
pub use devkit::executor::{
    Elapsed, Executor, JoinHandle, LocalExecutor, LocalSpawner, Receiver, Sender, Signal, Spawner, Timeout, Timer,
    local_spawner, spawner,
};

use crate::thread::{Builder, MAX_DELAY, NOTIFY_EXECUTOR, notify, ticks, wait_notify};
use crate::{Result, sys};

/// One spinlock for every executor; holds off interrupts and the other core.
pub struct Critical;

pub static CRITICAL: Critical = Critical;

impl CriticalSection for Critical {
    #[cfg(target_os = "none")]
    fn with(&self, f: &mut dyn FnMut()) {
        unsafe { sys::idf_executor_critical_enter() };
        f();
        unsafe { sys::idf_executor_critical_exit() };
    }

    // Host tests do not link the shim.
    #[cfg(not(target_os = "none"))]
    fn with(&self, f: &mut dyn FnMut()) {
        unsafe { sys::vPortEnterCritical() };
        f();
        unsafe { sys::vPortExitCritical() };
    }
}

/// Waits on the task that created it, for `NOTIFY_EXECUTOR`.
pub struct TaskPark {
    task: sys::TaskHandle_t,
}

unsafe impl Send for TaskPark {}
unsafe impl Sync for TaskPark {}

impl Park for TaskPark {
    fn park(&self, timeout: Option<u32>) {
        wait_notify(NOTIFY_EXECUTOR, timeout.map_or(MAX_DELAY, |t| t as sys::TickType_t));
    }

    fn unpark(&self) {
        notify(self.task, NOTIFY_EXECUTOR);
    }

    fn now(&self) -> u32 {
        unsafe { sys::xTaskGetTickCount() as u32 }
    }
}

/// A `Park` for the calling task.
pub fn park() -> Arc<dyn Park> {
    Arc::new(TaskPark { task: unsafe { sys::xTaskGetCurrentTaskHandle() } })
}

/// Runs `future` to completion on the calling task; see `devkit::executor::block_on`.
pub fn block_on<F: Future>(future: F) -> F::Output {
    devkit::executor::block_on(&CRITICAL, park(), future)
}

/// `devkit::executor::channel` on `CRITICAL`; `try_send` works from interrupts.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    devkit::executor::channel(&CRITICAL, capacity)
}

/// How long `ticks` of `park()`'s clock are.
pub fn tick_duration(ticks: u32) -> Duration {
    Duration::from_micros(u64::from(ticks) * 1_000_000 / sdkconfig::CONFIG_FREERTOS_HZ as u64)
}

pub mod oneshot {
    pub use devkit::executor::oneshot::{Closed, Receiver, Sender};

    /// `devkit::executor::oneshot::channel` on `CRITICAL`.
    pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
        devkit::executor::oneshot::channel(&super::CRITICAL)
    }
}

/// At least `d`, rounded like `thread::sleep`; on a devkit executor.
pub fn sleep(d: Duration) -> Timer {
    devkit::executor::sleep(ticks(d))
}

/// `future`'s output unless at least `d` passes first; the future is dropped then.
pub fn timeout<F: Future>(d: Duration, future: F) -> Timeout<F> {
    devkit::executor::timeout(ticks(d), future)
}

impl Builder {
    /// Starts a task running a `LocalExecutor` for good. `setup` runs on it
    /// first, to `spawn_local` what needs to stay there.
    pub fn spawn_local_executor(self, setup: impl FnOnce(&LocalExecutor) + Send + 'static) -> Result<Spawner> {
        let (tx, rx) = crate::sync::channel(1);
        self.spawn(move || {
            let local = LocalExecutor::new(&CRITICAL, park());
            tx.send(local.spawner());
            setup(&local);
            local.run()
        })?;
        Ok(rx.recv())
    }

    /// Starts one task per core running one `Executor` for good; the
    /// builder's core is ignored.
    pub fn spawn_executor(self) -> Result<Spawner> {
        let executor = Arc::new(Executor::new(&CRITICAL));
        for core in 0..crate::system::CORE_COUNT {
            let executor = executor.clone();
            self.clone().core(Some(core as u32)).spawn(move || executor.run(park()))?;
        }
        Ok(executor.spawner())
    }
}
