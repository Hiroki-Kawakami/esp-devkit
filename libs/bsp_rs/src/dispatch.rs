/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use core::time::Duration;

use idf::sync::{Mutex, OnceLock, Semaphore};
use idf::thread::Builder;
use idf::time::Instant;
use idf::Result;

const DEFAULT_PRIORITY: u8 = 5;
const STACK_SIZE: usize = 4096;
// Periods at least this long snap to multiples of themselves so harmonic periods
// share wakeups; shorter polls keep exact spacing.
const ALIGN_MIN: Duration = Duration::from_millis(100);
const NO_CORE: u32 = u32::MAX;

#[derive(Default)]
#[non_exhaustive]
pub struct Config {
    /// 0 selects 5.
    pub task_priority: u8,
    pub task_core: Option<u32>,
}

#[allow(dead_code)]
pub(crate) enum Next {
    Now,
    After(Duration),
    Idle,
    Done,
}

pub(crate) trait Source: Send {
    fn tick(&mut self) -> Next;
}

struct Slot {
    pending: AtomicBool,
}

#[derive(Clone)]
pub(crate) struct Notifier(Arc<Slot>);

#[allow(dead_code)]
impl Notifier {
    pub(crate) fn notify(&self) {
        self.0.pending.store(true, Ordering::Release);
        if let Some(d) = DISPATCHER.get() {
            d.wake.give();
        }
    }

    pub(crate) fn notify_from_isr(&self) {
        self.0.pending.store(true, Ordering::Release);
        if let Some(d) = DISPATCHER.get() {
            d.wake.give_from_isr();
        }
    }
}

struct Entry {
    source: Box<dyn Source>,
    slot: Arc<Slot>,
    deadline: Option<Instant>,
}

struct Dispatcher {
    wake: Semaphore,
    added: Mutex<Vec<Entry>>,
    has_added: AtomicBool,
}

static DISPATCHER: OnceLock<Arc<Dispatcher>> = OnceLock::new();
static PRIORITY: AtomicU8 = AtomicU8::new(0);
static CORE: AtomicU32 = AtomicU32::new(NO_CORE);

/// Only effective before the first `add`.
pub(crate) fn configure(config: &Config) {
    PRIORITY.store(config.task_priority, Ordering::Relaxed);
    CORE.store(config.task_core.unwrap_or(NO_CORE), Ordering::Relaxed);
}

#[allow(dead_code)]
pub(crate) fn add(source: Box<dyn Source>) -> Result<Notifier> {
    let d = DISPATCHER.get_or_try_init(start)?;
    let slot = Arc::new(Slot { pending: AtomicBool::new(true) });
    d.added.lock().push(Entry { source, slot: slot.clone(), deadline: None });
    d.has_added.store(true, Ordering::Release);
    d.wake.give();
    Ok(Notifier(slot))
}

fn start() -> Result<Arc<Dispatcher>> {
    let d = Arc::new(Dispatcher {
        wake: Semaphore::try_new(0, 1)?,
        added: Mutex::try_new(Vec::new())?,
        has_added: AtomicBool::new(false),
    });
    let priority = match PRIORITY.load(Ordering::Relaxed) {
        0 => DEFAULT_PRIORITY,
        p => p,
    };
    let core = match CORE.load(Ordering::Relaxed) {
        NO_CORE => None,
        c => Some(c),
    };
    let task = d.clone();
    Builder::new()
        .name("bsp_dispatch")
        .stack_size(STACK_SIZE)
        .priority(u32::from(priority))
        .core(core)
        .spawn(move || run(&task))?;
    Ok(d)
}

fn run(d: &Dispatcher) -> ! {
    let mut entries: Vec<Entry> = Vec::new();
    loop {
        if d.has_added.swap(false, Ordering::Acquire) {
            entries.append(&mut d.added.lock());
        }

        let now = Instant::now();
        let mut i = 0;
        while i < entries.len() {
            let entry = &mut entries[i];
            let due = entry.slot.pending.load(Ordering::Acquire)
                || entry.deadline.is_some_and(|d| d <= now);
            if due {
                // Cleared before tick, so a notify during tick runs it again.
                entry.slot.pending.store(false, Ordering::Release);
                entry.deadline = match entry.source.tick() {
                    Next::Now => Some(now),
                    Next::After(d) => Some(schedule(now, d)),
                    Next::Idle => None,
                    Next::Done => {
                        entries.swap_remove(i);
                        continue;
                    }
                };
            }
            i += 1;
        }

        match entries.iter().filter_map(|e| e.deadline).min() {
            Some(deadline) => {
                d.wake.take_deadline(deadline);
            }
            None => d.wake.take(),
        }
    }
}

fn schedule(now: Instant, after: Duration) -> Instant {
    if after < ALIGN_MIN {
        return now + after;
    }
    let period = after.as_micros() as u64;
    let elapsed = now.since_boot().as_micros() as u64;
    now + Duration::from_micros(period - elapsed % period)
}

#[cfg(test)]
mod tests {
    use super::{Next, Source, add, schedule};
    use alloc::boxed::Box;
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::time::Duration;
    use idf::thread::sleep;
    use idf::time::Instant;

    struct Counter {
        ticks: Arc<AtomicUsize>,
        next: fn(usize) -> Next,
    }

    impl Source for Counter {
        fn tick(&mut self) -> Next {
            let n = self.ticks.fetch_add(1, Ordering::SeqCst) + 1;
            (self.next)(n)
        }
    }

    fn counter(next: fn(usize) -> Next) -> (Arc<AtomicUsize>, Box<Counter>) {
        let ticks = Arc::new(AtomicUsize::new(0));
        (ticks.clone(), Box::new(Counter { ticks, next }))
    }

    #[test]
    fn periodic_source_repeats() {
        let (ticks, source) = counter(|_| Next::After(Duration::from_millis(20)));
        add(source).unwrap();
        sleep(Duration::from_millis(210));
        let n = ticks.load(Ordering::SeqCst);
        assert!((9..=12).contains(&n), "{n}");
    }

    #[test]
    fn idle_source_runs_on_add_and_notify_only() {
        let (ticks, source) = counter(|_| Next::Idle);
        let notifier = add(source).unwrap();
        sleep(Duration::from_millis(50));
        assert_eq!(ticks.load(Ordering::SeqCst), 1);
        notifier.notify();
        sleep(Duration::from_millis(50));
        assert_eq!(ticks.load(Ordering::SeqCst), 2);
        notifier.notify_from_isr();
        sleep(Duration::from_millis(50));
        assert_eq!(ticks.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn done_source_is_removed() {
        let (ticks, source) = counter(|n| if n < 3 { Next::Now } else { Next::Done });
        let notifier = add(source).unwrap();
        sleep(Duration::from_millis(50));
        assert_eq!(ticks.load(Ordering::SeqCst), 3);
        notifier.notify();
        sleep(Duration::from_millis(50));
        assert_eq!(ticks.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn sources_run_independently() {
        let (fast, a) = counter(|_| Next::After(Duration::from_millis(10)));
        let (idle, b) = counter(|_| Next::Idle);
        add(a).unwrap();
        add(b).unwrap();
        sleep(Duration::from_millis(105));
        assert!(fast.load(Ordering::SeqCst) >= 8);
        assert_eq!(idle.load(Ordering::SeqCst), 1);
    }

    struct Spawner(Arc<AtomicUsize>);

    impl Source for Spawner {
        fn tick(&mut self) -> Next {
            for _ in 0..32 {
                let ticks = self.0.clone();
                add(Box::new(Counter { ticks, next: |_| Next::Done })).unwrap();
            }
            Next::Done
        }
    }

    #[test]
    fn tick_can_add_many_sources() {
        let ticks = Arc::new(AtomicUsize::new(0));
        add(Box::new(Spawner(ticks.clone()))).unwrap();
        sleep(Duration::from_millis(50));
        assert_eq!(ticks.load(Ordering::SeqCst), 32);
    }

    #[test]
    fn long_periods_snap_to_their_multiples() {
        let now = Instant::now();
        let period = Duration::from_millis(250);
        let at = schedule(now, period);
        assert!(at > now && at <= now + period);
        assert_eq!(at.since_boot().as_micros() % period.as_micros(), 0);
        assert_eq!(schedule(now, Duration::from_millis(30)), now + Duration::from_millis(30));
    }
}
