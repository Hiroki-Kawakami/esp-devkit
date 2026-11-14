/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod mutex;
mod raw;
mod rwlock;
#[cfg(test)]
mod tests;

pub use mutex::{Mutex, MutexGuard, MutexLock};
pub use rwlock::{RwLock, RwLockRead, RwLockReadGuard, RwLockWrite, RwLockWriteGuard};

/// What the locks need from the platform's tasks. A higher priority is more
/// urgent.
pub trait Platform: 'static {
    /// The calling task, as `hold` and `unpark` take it.
    fn current() -> usize;
    fn priority() -> u32;
    /// Whether `task` is executing on another core at this moment.
    fn running_elsewhere(task: usize) -> bool;
    /// Returns once `unpark` targeted the calling task, also before this
    /// call; may return spuriously.
    fn park();
    /// Inside `critical`. `task` must stay able to take the `unpark` that
    /// follows, even if it stops waiting and ends meanwhile.
    fn hold(task: usize);
    /// Outside `critical`, once per `hold`.
    fn unpark(task: usize);
    /// Wraps.
    fn now_us() -> u32;
    /// Keeps out other tasks and cores. `f` neither blocks nor allocates.
    fn critical<R>(f: impl FnOnce() -> R) -> R;
}
