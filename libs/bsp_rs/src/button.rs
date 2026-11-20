/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
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

/// Buttons whose chip tells clicks apart itself. They report no levels: no
/// Down, Up or LongPress, and `is_pressed` stays false.
pub(crate) trait Judged: Send + 'static {
    fn count(&self) -> usize;
    /// Emits, by local id, what the chip judged since the last call. Runs on
    /// every wake of the buttons, so it returns early when nothing is pending.
    fn poll(&mut self, emit: &mut dyn FnMut(usize, Event)) -> Result<()>;
    /// `listening` while the app has a callback; `double` per local button.
    fn configure(&mut self, listening: bool, double: &[Option<Duration>]) -> Result<()>;
}

/// Ids run through the providers in the order given.
#[allow(dead_code)]
pub(crate) enum Provider {
    Level(Box<dyn Driver>),
    Judged(Box<dyn Judged>),
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
    config_changed: AtomicBool,
    notifier: OnceLock<Notifier>,
}

impl Buttons {
    fn new(count: u8) -> Self {
        Self {
            count,
            pressed: AtomicU8::new(0),
            injected: AtomicU8::new(0),
            double_ms: [const { AtomicU32::new(0) }; MAX_BUTTONS],
            long_ms: [const { AtomicU32::new(0) }; MAX_BUTTONS],
            callback: Mutex::new(None),
            callback_changed: AtomicBool::new(false),
            // A chip keeps what an earlier firmware set, so it hears the
            // defaults.
            config_changed: AtomicBool::new(true),
            notifier: OnceLock::new(),
        }
    }

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
        self.config_changed.store(true, Ordering::Release);
        self.wake();
    }

    /// `Some` turns double clicks on for `id`, `None` off. A button its chip
    /// judges takes the nearest interval the chip has.
    pub fn set_double_click(&self, id: u8, interval: Option<Duration>) {
        if let Some(slot) = self.double_ms.get(usize::from(id)) {
            slot.store(interval.map_or(0, |d| d.as_millis().max(1) as u32), Ordering::Release);
            self.config_changed.store(true, Ordering::Release);
            self.wake();
        }
    }

    /// `Some` turns long presses on for `id`, `None` off; no effect on a
    /// button its chip judges.
    pub fn set_long_press(&self, id: u8, duration: Option<Duration>) {
        if let Some(slot) = self.long_ms.get(usize::from(id)) {
            slot.store(duration.map_or(0, |d| d.as_millis().max(1) as u32), Ordering::Release);
        }
    }

    /// Holds `id` down, as a test harness does, until released the same way.
    /// On a button its chip judges, the release is a Click.
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

/// `notify` from an interrupt.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
pub(crate) fn notify_from_isr() {
    if let Some(n) = BUTTONS.get().and_then(|b| b.notifier.get()) {
        n.notify_from_isr();
    }
}

pub(crate) fn start(providers: Vec<Provider>) -> Result<&'static Buttons> {
    let (placed, count) = place(providers);
    let buttons = BUTTONS.get_or_init(|| Buttons::new(count));
    let notifier = dispatch::add(Box::new(ButtonSource::new(placed, buttons)))?;
    let _ = buttons.notifier.set(notifier);
    Ok(buttons)
}

struct Placed {
    base: usize,
    count: usize,
    provider: Provider,
}

fn place(providers: Vec<Provider>) -> (Vec<Placed>, u8) {
    let mut count = 0;
    let mut placed = Vec::new();
    for provider in providers {
        let n = match &provider {
            Provider::Level(d) => d.count(),
            Provider::Judged(d) => d.count(),
        };
        let n = n.min(MAX_BUTTONS - count);
        placed.push(Placed { base: count, count: n, provider });
        count += n;
    }
    (placed, count as u8)
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

struct ButtonSource {
    providers: Vec<Placed>,
    buttons: &'static Buttons,
    machines: [Machine; MAX_BUTTONS],
    callback: Option<EventCallback>,
    injected: u8,
    failing: bool,
}

impl ButtonSource {
    fn new(providers: Vec<Placed>, buttons: &'static Buttons) -> Self {
        Self { providers, buttons, machines: [Machine::new(); MAX_BUTTONS], callback: None, injected: 0, failing: false }
    }
}

fn duration(ms: &AtomicU32) -> Option<Duration> {
    let ms = ms.load(Ordering::Acquire);
    (ms > 0).then(|| Duration::from_millis(u64::from(ms)))
}

fn emit(buttons: &Buttons, callback: &mut Option<EventCallback>, id: usize, event: Event) {
    match event {
        Event::Down => buttons.pressed.fetch_or(1 << id, Ordering::AcqRel),
        Event::Up => buttons.pressed.fetch_and(!(1 << id), Ordering::AcqRel),
        _ => 0,
    };
    if let Some(cb) = callback {
        cb(id as u8, event);
    }
}

impl Source for ButtonSource {
    fn tick(&mut self) -> Next {
        let buttons = self.buttons;
        if buttons.callback_changed.swap(false, Ordering::Acquire) {
            if let Some(cb) = buttons.callback.lock().take() {
                self.callback = cb;
            }
        }
        let configure = buttons.config_changed.swap(false, Ordering::AcqRel);
        let listening = self.callback.is_some();
        let doubles: [Option<Duration>; MAX_BUTTONS] = core::array::from_fn(|id| duration(&buttons.double_ms[id]));
        let injected = buttons.injected.load(Ordering::Acquire);
        let released = self.injected & !injected;
        self.injected = injected;
        let now = Instant::now();
        let mut settled = true;
        let mut polled = false;
        let mut failed = None;
        for placed in &mut self.providers {
            let (base, count) = (placed.base, placed.count);
            let callback = &mut self.callback;
            match &mut placed.provider {
                Provider::Level(driver) => {
                    let mut levels = [false; MAX_BUTTONS];
                    if let Err(err) = driver.sample(&mut levels[..count]) {
                        failed = Some(("sample", err));
                        continue;
                    }
                    polled |= !driver.notifies();
                    for (i, &level) in levels[..count].iter().enumerate() {
                        let id = base + i;
                        let raw = level || injected & 1 << id != 0;
                        let long = duration(&buttons.long_ms[id]);
                        settled &= self.machines[id].step(raw, now, doubles[id], long, &mut |e| emit(buttons, callback, id, e));
                    }
                }
                Provider::Judged(driver) => {
                    if configure && let Err(err) = driver.configure(listening, &doubles[base..base + count]) {
                        buttons.config_changed.store(true, Ordering::Release);
                        failed = Some(("configure", err));
                    }
                    if let Err(err) = driver.poll(&mut |i, e| emit(buttons, callback, base + i, e)) {
                        failed = Some(("poll", err));
                    }
                    for id in (base..base + count).filter(|id| released & 1 << id != 0) {
                        emit(buttons, callback, id, Event::Click);
                    }
                }
            }
        }
        if let Some((what, err)) = failed {
            if !self.failing {
                warn!(TAG, "{what}: {err}");
                self.failing = true;
            }
            return Next::After(POLL_INTERVAL);
        }
        self.failing = false;
        if !settled || polled { Next::After(POLL_INTERVAL) } else { Next::Idle }
    }
}

#[cfg(test)]
mod tests {
    use super::{ButtonSource, Buttons, Driver, Event, Judged, Machine, Provider, place};
    use crate::dispatch::Source;
    use alloc::boxed::Box;
    use alloc::sync::Arc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::time::Duration;
    use idf::Result;
    use idf::sync::Mutex;
    use idf::time::Instant;

    struct Released(usize);

    impl Driver for Released {
        fn count(&self) -> usize {
            self.0
        }

        fn sample(&mut self, pressed: &mut [bool]) -> Result<()> {
            pressed.fill(false);
            Ok(())
        }

        fn notifies(&self) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct Chip {
        pending: Vec<(usize, Event)>,
        configured: Vec<(bool, Vec<Option<Duration>>)>,
    }

    struct FakeJudged(Arc<Mutex<Chip>>);

    impl Judged for FakeJudged {
        fn count(&self) -> usize {
            1
        }

        fn poll(&mut self, emit: &mut dyn FnMut(usize, Event)) -> Result<()> {
            for (id, event) in self.0.lock().pending.drain(..) {
                emit(id, event);
            }
            Ok(())
        }

        fn configure(&mut self, listening: bool, double: &[Option<Duration>]) -> Result<()> {
            self.0.lock().configured.push((listening, double.to_vec()));
            Ok(())
        }
    }

    type Log = Arc<Mutex<Vec<(u8, Event)>>>;

    fn mixed() -> (ButtonSource, &'static Buttons, Arc<Mutex<Chip>>, Log) {
        let chip = Arc::new(Mutex::new(Chip::default()));
        let (placed, count) = place(vec![Provider::Level(Box::new(Released(2))), Provider::Judged(Box::new(FakeJudged(chip.clone())))]);
        let buttons: &'static Buttons = Box::leak(Box::new(Buttons::new(count)));
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        buttons.set_event_cb(Some(Box::new(move |id, event| sink.lock().push((id, event)))));
        (ButtonSource::new(placed, buttons), buttons, chip, log)
    }

    #[test]
    fn a_judged_provider_takes_the_ids_after_the_level_ones() {
        let (mut src, buttons, chip, log) = mixed();
        assert_eq!(buttons.count(), 3);
        chip.lock().pending.push((0, Event::DoubleClick));
        src.tick();
        assert_eq!(*log.lock(), [(2, Event::DoubleClick)]);
        assert!(!buttons.is_pressed(2));
    }

    #[test]
    fn a_judged_provider_hears_the_callback_and_its_own_double_click() {
        let (mut src, buttons, chip, _) = mixed();
        src.tick();
        buttons.set_double_click(2, Some(Duration::from_millis(300)));
        src.tick();
        src.tick();
        buttons.set_event_cb(None);
        src.tick();
        let configured = &chip.lock().configured;
        assert_eq!(*configured, [(true, vec![None]), (true, vec![Some(Duration::from_millis(300))]), (false, vec![Some(Duration::from_millis(300))])]);
    }

    #[test]
    fn an_injected_press_of_a_judged_button_clicks_on_release() {
        let (mut src, buttons, _, log) = mixed();
        buttons.inject(2, true);
        src.tick();
        assert!(log.lock().is_empty());
        buttons.inject(2, false);
        src.tick();
        assert_eq!(*log.lock(), [(2, Event::Click)]);
    }

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
