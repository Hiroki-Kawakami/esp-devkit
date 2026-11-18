/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use core::time::Duration;

use idf::Result;
use idf::log::warn;
use idf::sync::{Mutex, OnceLock};
use idf::time::Instant;

use crate::dispatch::{self, Next, Notifier, Source};

const TAG: &CStr = c"button";

pub const MAX_BUTTONS: usize = 8;
const DEBOUNCE: Duration = Duration::from_millis(20);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// Every debounced press.
    Down,
    /// Every debounced release.
    Up,
    /// A press and release, without a long press. With double clicks on, it
    /// comes once the interval passes without a second press.
    Click,
    /// The second press within the interval, instead of two clicks.
    DoubleClick,
    /// The press held for its duration; no click follows.
    LongPress,
}

pub type EventCallback = Box<dyn FnMut(u8, Event) + Send>;

pub(crate) trait Driver: Send + 'static {
    fn count(&self) -> usize;
    /// Levels into `pressed`, one per button.
    fn sample(&mut self, pressed: &mut [bool]) -> Result<()>;
    /// Whether it calls `notify` when a level changes, so nothing polls it
    /// while every button rests.
    fn notifies(&self) -> bool {
        false
    }
}

pub struct Buttons {
    count: u8,
    pressed: AtomicU8,
    injected: AtomicU8,
    /// Per button, 0 for off.
    double_ms: [AtomicU32; MAX_BUTTONS],
    long_ms: [AtomicU32; MAX_BUTTONS],
    callback: Mutex<Option<Option<EventCallback>>>,
    callback_changed: AtomicBool,
    notifier: OnceLock<Notifier>,
}

impl Buttons {
    pub fn count(&self) -> usize {
        usize::from(self.count)
    }

    /// The debounced level of `id`.
    pub fn is_pressed(&self, id: u8) -> bool {
        self.pressed.load(Ordering::Acquire) & 1 << id != 0
    }

    /// Runs on the dispatch task with each button's events. Takes effect from
    /// the next tick, so the previous callback may still run once after this
    /// returns.
    pub fn set_event_cb(&self, callback: Option<EventCallback>) {
        *self.callback.lock() = Some(callback);
        self.callback_changed.store(true, Ordering::Release);
        self.wake();
    }

    /// `Some` turns double clicks on for `id`, `None` off.
    pub fn set_double_click(&self, id: u8, interval: Option<Duration>) {
        if let Some(slot) = self.double_ms.get(usize::from(id)) {
            slot.store(interval.map_or(0, |d| d.as_millis().max(1) as u32), Ordering::Release);
        }
    }

    /// `Some` turns long presses on for `id`, `None` off.
    pub fn set_long_press(&self, id: u8, duration: Option<Duration>) {
        if let Some(slot) = self.long_ms.get(usize::from(id)) {
            slot.store(duration.map_or(0, |d| d.as_millis().max(1) as u32), Ordering::Release);
        }
    }

    /// Holds `id` down, as a test harness does, until released the same way.
    pub fn inject(&self, id: u8, pressed: bool) {
        if id >= self.count {
            return;
        }
        if pressed {
            self.injected.fetch_or(1 << id, Ordering::AcqRel);
        } else {
            self.injected.fetch_and(!(1 << id), Ordering::AcqRel);
        }
        self.wake();
    }

    fn wake(&self) {
        if let Some(n) = self.notifier.get() {
            n.notify();
        }
    }
}

static BUTTONS: OnceLock<Buttons> = OnceLock::new();

/// Wakes the buttons for a driver that `notifies`.
pub(crate) fn notify() {
    if let Some(buttons) = BUTTONS.get() {
        buttons.wake();
    }
}

pub(crate) fn start<D: Driver>(driver: D) -> Result<&'static Buttons> {
    let count = driver.count().min(MAX_BUTTONS) as u8;
    let buttons = BUTTONS.get_or_init(|| Buttons {
        count,
        pressed: AtomicU8::new(0),
        injected: AtomicU8::new(0),
        double_ms: [const { AtomicU32::new(0) }; MAX_BUTTONS],
        long_ms: [const { AtomicU32::new(0) }; MAX_BUTTONS],
        callback: Mutex::new(None),
        callback_changed: AtomicBool::new(false),
        notifier: OnceLock::new(),
    });
    let notifier = dispatch::add(Box::new(ButtonSource::new(driver, buttons)))?;
    let _ = buttons.notifier.set(notifier);
    Ok(buttons)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Idle,
    Pressed,
    PressedLong,
    WaitDouble,
    PressedAgain,
}

#[derive(Clone, Copy)]
struct Machine {
    stable: bool,
    differs_since: Option<Instant>,
    state: State,
    pressed_at: Instant,
    released_at: Instant,
}

impl Machine {
    fn new() -> Self {
        let now = Instant::now();
        Self { stable: false, differs_since: None, state: State::Idle, pressed_at: now, released_at: now }
    }

    /// Advances from `raw` at `now`; `true` once nothing waits on time.
    fn step(&mut self, raw: bool, now: Instant, double: Option<Duration>, long: Option<Duration>, emit: &mut impl FnMut(Event)) -> bool {
        let mut edge = false;
        if raw == self.stable {
            self.differs_since = None;
        } else {
            let since = *self.differs_since.get_or_insert(now);
            if now.duration_since(since) >= DEBOUNCE {
                (self.stable, self.differs_since, edge) = (raw, None, true);
            }
        }
        if edge && raw {
            emit(Event::Down);
            self.pressed_at = now;
            if self.state == State::WaitDouble {
                emit(Event::DoubleClick);
                self.state = State::PressedAgain;
            } else {
                self.state = State::Pressed;
            }
        } else if edge {
            emit(Event::Up);
            self.state = match (self.state, double) {
                (State::Pressed, Some(_)) => {
                    self.released_at = now;
                    State::WaitDouble
                }
                (State::Pressed, None) => {
                    emit(Event::Click);
                    State::Idle
                }
                _ => State::Idle,
            };
        }
        match self.state {
            State::Pressed if long.is_some_and(|l| now.duration_since(self.pressed_at) >= l) => {
                emit(Event::LongPress);
                self.state = State::PressedLong;
            }
            State::WaitDouble if double.is_none_or(|d| now.duration_since(self.released_at) >= d) => {
                emit(Event::Click);
                self.state = State::Idle;
            }
            _ => {}
        }
        self.state == State::Idle && self.differs_since.is_none()
    }
}

struct ButtonSource<D> {
    driver: D,
    buttons: &'static Buttons,
    machines: [Machine; MAX_BUTTONS],
    callback: Option<EventCallback>,
    failing: bool,
}

impl<D: Driver> ButtonSource<D> {
    fn new(driver: D, buttons: &'static Buttons) -> Self {
        Self { driver, buttons, machines: [Machine::new(); MAX_BUTTONS], callback: None, failing: false }
    }
}

fn duration(ms: &AtomicU32) -> Option<Duration> {
    let ms = ms.load(Ordering::Acquire);
    (ms > 0).then(|| Duration::from_millis(u64::from(ms)))
}

impl<D: Driver> Source for ButtonSource<D> {
    fn tick(&mut self) -> Next {
        let buttons = self.buttons;
        if buttons.callback_changed.swap(false, Ordering::Acquire) {
            if let Some(cb) = buttons.callback.lock().take() {
                self.callback = cb;
            }
        }
        let count = usize::from(buttons.count);
        let mut levels = [false; MAX_BUTTONS];
        if let Err(err) = self.driver.sample(&mut levels[..count]) {
            if !self.failing {
                warn!(TAG, "sample: {err}");
                self.failing = true;
            }
            return Next::After(POLL_INTERVAL);
        }
        self.failing = false;
        let injected = buttons.injected.load(Ordering::Acquire);
        let now = Instant::now();
        let mut settled = true;
        for (id, (machine, &level)) in self.machines.iter_mut().zip(&levels).take(count).enumerate() {
            let callback = &mut self.callback;
            let mut emit = |event| {
                match event {
                    Event::Down => buttons.pressed.fetch_or(1 << id, Ordering::AcqRel),
                    Event::Up => buttons.pressed.fetch_and(!(1 << id), Ordering::AcqRel),
                    _ => 0,
                };
                if let Some(cb) = callback {
                    cb(id as u8, event);
                }
            };
            let raw = level || injected & 1 << id != 0;
            settled &= machine.step(raw, now, duration(&buttons.double_ms[id]), duration(&buttons.long_ms[id]), &mut emit);
        }
        if !settled || !self.driver.notifies() { Next::After(POLL_INTERVAL) } else { Next::Idle }
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, Machine};
    use alloc::vec::Vec;
    use core::time::Duration;
    use idf::time::Instant;

    fn run(machine: &mut Machine, steps: &[(u64, bool)], double: Option<u64>, long: Option<u64>) -> Vec<(u64, Event)> {
        let start = Instant::now();
        let mut out = Vec::new();
        for &(ms, raw) in steps {
            let mut emit = |e| out.push((ms, e));
            machine.step(raw, start + Duration::from_millis(ms), double.map(Duration::from_millis), long.map(Duration::from_millis), &mut emit);
        }
        out
    }

    fn press(from: u64, to: u64, end: u64) -> Vec<(u64, bool)> {
        (0..=end).step_by(10).map(|ms| (ms, (from..to).contains(&ms))).collect()
    }

    #[test]
    fn a_short_press_clicks_on_release_after_the_debounce() {
        let events = run(&mut Machine::new(), &press(0, 100, 200), None, None);
        assert_eq!(events, [(20, Event::Down), (120, Event::Up), (120, Event::Click)]);
    }

    #[test]
    fn a_bounce_shorter_than_the_debounce_is_ignored() {
        let events = run(&mut Machine::new(), &[(0, true), (10, false), (20, false), (30, false)], None, None);
        assert!(events.is_empty());
    }

    #[test]
    fn a_held_press_fires_a_long_press_and_no_click() {
        let events = run(&mut Machine::new(), &press(0, 1000, 1100), None, Some(800));
        assert_eq!(events, [(20, Event::Down), (820, Event::LongPress), (1020, Event::Up)]);
    }

    #[test]
    fn with_double_clicks_on_a_click_waits_out_the_interval() {
        let events = run(&mut Machine::new(), &press(0, 100, 500), Some(300), None);
        assert_eq!(events, [(20, Event::Down), (120, Event::Up), (420, Event::Click)]);
    }

    #[test]
    fn a_second_press_within_the_interval_is_a_double_click() {
        let mut steps = press(0, 100, 150);
        steps.extend((160..=400).step_by(10).map(|ms| (ms, (200..300).contains(&ms))));
        let events = run(&mut Machine::new(), &steps, Some(300), None);
        assert_eq!(events, [(20, Event::Down), (120, Event::Up), (220, Event::Down), (220, Event::DoubleClick), (320, Event::Up)]);
    }
}
