/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::cell::Cell;
use std::future::{Future, poll_fn};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use super::{
    CriticalSection, Elapsed, Executor, LocalExecutor, Park, Signal, block_on, channel, local_spawner, oneshot, sleep,
    spawner, timeout,
};

struct StdCritical(Mutex<()>);

impl CriticalSection for StdCritical {
    fn with(&self, f: &mut dyn FnMut()) {
        let _guard = self.0.lock().unwrap();
        f();
    }
}

static CRITICAL: StdCritical = StdCritical(Mutex::new(()));

#[derive(Default)]
struct StdPark {
    token: Mutex<bool>,
    cond: Condvar,
}

impl Park for StdPark {
    fn park(&self, timeout: Option<u32>) {
        let mut token = self.token.lock().unwrap();
        while !*token {
            match timeout {
                None => token = self.cond.wait(token).unwrap(),
                Some(ms) => {
                    let (t, result) = self.cond.wait_timeout(token, Duration::from_millis(ms.into())).unwrap();
                    token = t;
                    if result.timed_out() {
                        break;
                    }
                }
            }
        }
        *token = false;
    }

    fn unpark(&self) {
        *self.token.lock().unwrap() = true;
        self.cond.notify_one();
    }

    fn now(&self) -> u32 {
        EPOCH.elapsed().as_millis() as u32
    }
}

static EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

fn park() -> Arc<dyn Park> {
    Arc::new(StdPark::default())
}

/// Pending until `set` is called from anywhere.
#[derive(Clone, Default)]
struct Flag(Arc<Mutex<(bool, Option<Waker>)>>);

impl Flag {
    fn set(&self) {
        let waker = {
            let mut state = self.0.lock().unwrap();
            state.0 = true;
            state.1.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn wait(&self) -> impl Future<Output = ()> + Send + 'static {
        let flag = self.clone();
        poll_fn(move |cx| {
            let mut state = flag.0.lock().unwrap();
            if state.0 {
                Poll::Ready(())
            } else {
                state.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        })
    }
}

struct DropCount(Arc<AtomicUsize>);

impl Drop for DropCount {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn block_on_returns_the_output() {
    assert_eq!(block_on(&CRITICAL, park(), async { 7 }), 7);
}

#[test]
fn spawn_and_join() {
    let out = block_on(&CRITICAL, park(), async {
        let spawner = spawner().await;
        let a = spawner.spawn(async { 1 });
        let b = spawner.spawn(async { 2 });
        a.await + b.await
    });
    assert_eq!(out, 3);
}

#[test]
fn wake_from_another_thread() {
    let flag = Flag::default();
    let setter = flag.clone();
    let out = block_on(&CRITICAL, park(), async move {
        let task = spawner().await.spawn({
            let wait = flag.wait();
            async move {
                wait.await;
                5
            }
        });
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            setter.set();
        });
        task.await
    });
    assert_eq!(out, 5);
}

#[test]
fn local_tasks_need_not_be_send() {
    let out = block_on(&CRITICAL, park(), async {
        let shared = Rc::new(Cell::new(0));
        let local = local_spawner().await;
        let tasks: Vec<_> = (1..=4)
            .map(|i| {
                let shared = shared.clone();
                local.spawn_local(async move { shared.set(shared.get() + i) })
            })
            .collect();
        for task in tasks {
            task.await;
        }
        shared.get()
    });
    assert_eq!(out, 10);
}

#[test]
fn abort_drops_the_future() {
    let drops = Arc::new(AtomicUsize::new(0));
    let flag = Flag::default();
    let out = block_on(&CRITICAL, park(), {
        let drops = drops.clone();
        async move {
            let spawner = spawner().await;
            let guard = DropCount(drops.clone());
            let wait = flag.wait();
            let task = spawner.spawn(async move {
                let _guard = guard;
                wait.await;
            });
            let yielded = Flag::default();
            let resume = yielded.clone();
            spawner.spawn(async move { resume.set() });
            yielded.wait().await;
            assert!(!task.is_finished());
            task.abort();
            let after = Flag::default();
            let resume = after.clone();
            spawner.spawn(async move { resume.set() });
            after.wait().await;
            drops.load(Ordering::SeqCst)
        }
    });
    assert_eq!(out, 1);
}

#[test]
fn detached_task_runs_on() {
    let done = Arc::new(AtomicBool::new(false));
    let flag = Flag::default();
    block_on(&CRITICAL, park(), {
        let done = done.clone();
        async move {
            let spawner = spawner().await;
            let finished = flag.clone();
            drop(spawner.spawn(async move {
                done.store(true, Ordering::SeqCst);
                finished.set();
            }));
            flag.wait().await;
        }
    });
    assert!(done.load(Ordering::SeqCst));
}

#[test]
fn dropping_the_executor_drops_pending_tasks() {
    let drops = Arc::new(AtomicUsize::new(0));
    let never = Flag::default();
    let spawn_park = park();
    let executor = LocalExecutor::new(&CRITICAL, spawn_park);
    for _ in 0..3 {
        let guard = DropCount(drops.clone());
        let wait = never.wait();
        drop(executor.spawn_local(async move {
            let _guard = guard;
            wait.await;
        }));
    }
    executor.run_until(async {
        let yielded = Flag::default();
        let resume = yielded.clone();
        spawner().await.spawn(async move { resume.set() });
        yielded.wait().await;
    });
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(executor);
    assert_eq!(drops.load(Ordering::SeqCst), 3);
    never.set();
}

#[test]
fn spawn_from_another_thread_into_a_running_local_executor() {
    let executor = LocalExecutor::new(&CRITICAL, park());
    let spawner = executor.spawner();
    let out = executor.run_until(async move {
        let handle = thread::spawn(move || spawner.spawn(async { 9 })).join().unwrap();
        handle.await
    });
    assert_eq!(out, 9);
}

#[test]
fn multi_runner_executor_spreads_work() {
    static EXECUTOR: std::sync::LazyLock<Executor> = std::sync::LazyLock::new(|| Executor::new(&CRITICAL));
    let runners: Vec<_> = (0..2)
        .map(|_| {
            thread::spawn(|| -> () { EXECUTOR.run(park()) })
        })
        .collect();
    let seen = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let count = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..200)
        .map(|_| {
            let seen = seen.clone();
            let count = count.clone();
            EXECUTOR.spawn(async move {
                seen.lock().unwrap().insert(thread::current().id());
                let start = Instant::now();
                while start.elapsed() < Duration::from_micros(200) {}
                count.fetch_add(1, Ordering::SeqCst)
            })
        })
        .collect();
    let total: usize = block_on(&CRITICAL, park(), async move {
        let mut sum = 0;
        for handle in handles {
            sum += handle.await;
        }
        sum
    });
    assert_eq!(count.load(Ordering::SeqCst), 200);
    assert_eq!(total, (0..200).sum());
    assert_eq!(seen.lock().unwrap().len(), 2);
    drop(runners);
}

#[test]
fn many_wakes_from_many_threads() {
    let out = block_on(&CRITICAL, park(), async {
        let spawner = spawner().await;
        let mut handles = Vec::new();
        for i in 0..50 {
            let flag = Flag::default();
            let setter = flag.clone();
            let wait = flag.wait();
            handles.push(spawner.spawn(async move {
                wait.await;
                i
            }));
            thread::spawn(move || setter.set());
        }
        let mut sum = 0;
        for handle in handles {
            sum += handle.await;
        }
        sum
    });
    assert_eq!(out, (0..50).sum());
}

#[test]
#[should_panic(expected = "not on a LocalExecutor")]
fn local_spawner_on_a_multi_runner_executor_panics() {
    let executor = Executor::new(&CRITICAL);
    drop(executor.spawn(async { drop(local_spawner().await) }));
    let runner = executor.inner.register_runner(park());
    let task = executor.inner.next(runner).unwrap();
    executor.inner.run_task(task);
}

#[test]
fn sleep_waits_at_least_its_ticks() {
    let start = Instant::now();
    block_on(&CRITICAL, park(), sleep(30));
    assert!(start.elapsed() >= Duration::from_millis(30));
}

#[test]
fn timers_fire_in_deadline_order() {
    let order = block_on(&CRITICAL, park(), async {
        let spawner = spawner().await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let handles: Vec<_> = [40u32, 10, 30, 20]
            .into_iter()
            .map(|ticks| {
                let order = order.clone();
                spawner.spawn(async move {
                    sleep(ticks).await;
                    order.lock().unwrap().push(ticks);
                })
            })
            .collect();
        for handle in handles {
            handle.await;
        }
        Arc::try_unwrap(order).unwrap().into_inner().unwrap()
    });
    assert_eq!(order, [10, 20, 30, 40]);
}

#[test]
fn timeout_drops_a_late_future() {
    let drops = Arc::new(AtomicUsize::new(0));
    let never = Flag::default();
    let result = block_on(&CRITICAL, park(), {
        let guard = DropCount(drops.clone());
        let wait = never.wait();
        timeout(20, async move {
            let _guard = guard;
            wait.await;
        })
    });
    assert_eq!(result, Err(Elapsed));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(block_on(&CRITICAL, park(), timeout(200, async { sleep(5).await; 3 })), Ok(3));
}

#[test]
fn aborting_a_sleeper_leaves_other_timers_working() {
    let woke = block_on(&CRITICAL, park(), async {
        let spawner = spawner().await;
        let sleeper = spawner.spawn(sleep(1000));
        let short = spawner.spawn(async { sleep(10).await; true });
        sleep(1).await;
        sleeper.abort();
        short.await
    });
    assert!(woke);
}

#[test]
fn timers_on_a_multi_runner_executor() {
    static EXECUTOR: std::sync::LazyLock<Executor> = std::sync::LazyLock::new(|| Executor::new(&CRITICAL));
    for _ in 0..2 {
        thread::spawn(|| -> () { EXECUTOR.run(park()) });
    }
    let start = Instant::now();
    let handles: Vec<_> = (0..20).map(|i| EXECUTOR.spawn(async move { sleep(20 + i).await })).collect();
    block_on(&CRITICAL, park(), async move {
        for handle in handles {
            handle.await;
        }
    });
    assert!(start.elapsed() >= Duration::from_millis(39));
}

#[test]
fn signal_keeps_the_latest_value() {
    static SIGNAL: Signal<u32> = Signal::new(&CRITICAL);
    SIGNAL.signal(1);
    SIGNAL.signal(2);
    assert_eq!(block_on(&CRITICAL, park(), SIGNAL.wait()), 2);
    assert_eq!(SIGNAL.try_take(), None);
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(20));
        SIGNAL.signal(3);
    });
    assert_eq!(block_on(&CRITICAL, park(), SIGNAL.wait()), 3);
}

#[test]
fn channel_from_threads_keeps_each_senders_order() {
    let (tx, mut rx) = channel::<(u32, u32)>(&CRITICAL, 4);
    for producer in 0..4 {
        let tx = tx.clone();
        thread::spawn(move || {
            for i in 0..100 {
                let mut value = (producer, i);
                loop {
                    match tx.try_send(value) {
                        Ok(()) => break,
                        Err(back) => {
                            value = back;
                            thread::yield_now();
                        }
                    }
                }
            }
        });
    }
    let last = block_on(&CRITICAL, park(), async move {
        let mut last = [None::<u32>; 4];
        for _ in 0..400 {
            let (producer, i) = rx.recv().await;
            let slot = &mut last[producer as usize];
            assert!(slot.is_none_or(|prev| prev + 1 == i));
            *slot = Some(i);
        }
        last
    });
    assert_eq!(last, [Some(99); 4]);
}

#[test]
fn try_send_reports_a_full_channel() {
    let (tx, rx) = channel(&CRITICAL, 2);
    assert_eq!(tx.try_send(1), Ok(()));
    assert_eq!(tx.try_send(2), Ok(()));
    assert_eq!(tx.try_send(3), Err(3));
    assert_eq!(rx.try_recv(), Some(1));
    assert_eq!(tx.try_send(3), Ok(()));
}

#[test]
fn async_send_waits_for_room_in_order() {
    let got = block_on(&CRITICAL, park(), async {
        let spawner = spawner().await;
        let (tx, mut rx) = channel::<u32>(&CRITICAL, 1);
        let senders: Vec<_> = (0..5)
            .map(|i| {
                let tx = tx.clone();
                spawner.spawn(async move { tx.send(i).await })
            })
            .collect();
        sleep(5).await;
        let mut got = Vec::new();
        for _ in 0..5 {
            got.push(rx.recv().await);
        }
        for sender in senders {
            sender.await;
        }
        got
    });
    assert_eq!(got, [0, 1, 2, 3, 4]);
}

#[test]
fn dropping_a_waiting_send_passes_the_turn_on() {
    let got = block_on(&CRITICAL, park(), async {
        let spawner = spawner().await;
        let (tx, mut rx) = channel::<u32>(&CRITICAL, 1);
        tx.try_send(0).unwrap();
        let first = spawner.spawn({
            let tx = tx.clone();
            async move { tx.send(1).await }
        });
        sleep(2).await;
        let second = spawner.spawn({
            let tx = tx.clone();
            async move { tx.send(2).await }
        });
        sleep(2).await;
        first.abort();
        sleep(2).await;
        let a = rx.recv().await;
        let b = rx.recv().await;
        second.await;
        [a, b]
    });
    assert_eq!(got, [0, 2]);
}

#[test]
fn closing_a_channel_refuses_and_releases_senders() {
    let got = block_on(&CRITICAL, park(), async {
        let spawner = spawner().await;
        let (tx, rx) = channel::<u32>(&CRITICAL, 1);
        tx.try_send(0).unwrap();
        let waiting = spawner.spawn({
            let tx = tx.clone();
            async move { tx.send(1).await }
        });
        sleep(2).await;
        assert!(!tx.is_closed());
        drop(rx);
        waiting.await;
        (tx.is_closed(), tx.try_send(2))
    });
    assert_eq!(got, (true, Err(2)));
}

#[test]
fn oneshot_delivers_or_reports_the_other_side_gone() {
    let (tx, rx) = oneshot::channel::<u32>(&CRITICAL);
    let sender = thread::spawn(move || {
        thread::sleep(Duration::from_millis(5));
        tx.send(7)
    });
    assert_eq!(block_on(&CRITICAL, park(), rx), Ok(7));
    assert_eq!(sender.join().unwrap(), Ok(()));

    let (tx, rx) = oneshot::channel::<u32>(&CRITICAL);
    drop(tx);
    assert_eq!(block_on(&CRITICAL, park(), rx), Err(oneshot::Closed));

    let (tx, rx) = oneshot::channel::<u32>(&CRITICAL);
    assert!(!tx.is_closed());
    drop(rx);
    assert!(tx.is_closed());
    assert_eq!(tx.send(1), Err(1));
}

struct Unparks(AtomicUsize);

impl Park for Unparks {
    fn park(&self, _: Option<u32>) {
        unreachable!();
    }

    fn unpark(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn now(&self) -> u32 {
        EPOCH.elapsed().as_millis() as u32
    }
}

#[test]
fn run_ready_runs_what_is_ready_and_unparks_for_the_rest() {
    let park = Arc::new(Unparks(AtomicUsize::new(0)));
    let local = LocalExecutor::new(&CRITICAL, park.clone());
    let flag = Flag::default();
    let steps = Rc::new(Cell::new(0));
    local.spawn_local({
        let (flag, steps) = (flag.clone(), steps.clone());
        async move {
            steps.set(1);
            flag.wait().await;
            steps.set(2);
            sleep(20).await;
            steps.set(3);
        }
    });
    assert_eq!(local.run_ready(), None);
    assert_eq!(steps.get(), 1);
    let unparked = park.0.load(Ordering::SeqCst);
    let setter = thread::spawn(move || flag.set());
    setter.join().unwrap();
    assert_eq!(park.0.load(Ordering::SeqCst), unparked + 1, "a wake while it is not running unparks");
    let wait = local.run_ready().expect("the sleep's deadline");
    assert_eq!(steps.get(), 2);
    assert!(wait <= 20);
    thread::sleep(Duration::from_millis(u64::from(wait) + 2));
    assert_eq!(local.run_ready(), None);
    assert_eq!(steps.get(), 3);
}
