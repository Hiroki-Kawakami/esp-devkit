/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering, fence};
use core::time::Duration;

use idf::log::{error, warn};
use idf::sync::{Mutex, OnceLock, Semaphore};
use idf::Result;

use crate::dispatch::{self, Next, Notifier, Source};

const TAG: &CStr = c"touch";

pub const MAX_POINTS: usize = 10;
const DEFAULT_SETTLE_COUNT: u8 = 5;
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Point {
    pub x: u16,
    pub y: u16,
    /// Track id from the controller.
    pub id: u8,
}

pub type EventCallback = Box<dyn FnMut(&[Point]) + Send>;

pub(crate) trait Driver: Send + 'static {
    /// The latest known touches in chip space. A chip with a data-ready
    /// handshake returns its last frame when nothing new arrived.
    fn poll(&mut self) -> Result<&[Point]>;
}

pub(crate) struct Config {
    /// Display space, for mirroring.
    pub width: u16,
    pub height: u16,
    pub swap_xy: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
    /// Falling edge on it marks a new sample; `None` polls continuously.
    pub int_pin: Option<i32>,
}

struct Snapshot {
    seq: AtomicU32,
    count: AtomicU8,
    xy: [AtomicU32; MAX_POINTS],
    id: [AtomicU8; MAX_POINTS],
}

impl Snapshot {
    const fn new() -> Self {
        Self {
            seq: AtomicU32::new(0),
            count: AtomicU8::new(0),
            xy: [const { AtomicU32::new(0) }; MAX_POINTS],
            id: [const { AtomicU8::new(0) }; MAX_POINTS],
        }
    }
}

pub struct Touch {
    // Double-buffered so a reader never waits on the writer: a single seqlock
    // would spin forever when the reader preempts the writer on the same core.
    snapshots: [Snapshot; 2],
    active: AtomicUsize,
    wait: OnceLock<Semaphore>,
    int_pending: AtomicBool,
    callback: Mutex<Option<Option<EventCallback>>>,
    callback_changed: AtomicBool,
    notifier: OnceLock<Notifier>,
}

impl Touch {
    fn new() -> Result<Self> {
        Ok(Self {
            snapshots: [Snapshot::new(), Snapshot::new()],
            active: AtomicUsize::new(0),
            wait: OnceLock::new(),
            int_pending: AtomicBool::new(false),
            callback: Mutex::try_new(None)?,
            callback_changed: AtomicBool::new(false),
            notifier: OnceLock::new(),
        })
    }

    /// The latest sample in display space; 0 while nothing touches.
    pub fn read(&self, points: &mut [Point]) -> usize {
        loop {
            let snapshot = &self.snapshots[self.active.load(Ordering::Acquire)];
            let seq = snapshot.seq.load(Ordering::Acquire);
            let n = usize::from(snapshot.count.load(Ordering::Relaxed)).min(points.len());
            for (i, p) in points[..n].iter_mut().enumerate() {
                let xy = snapshot.xy[i].load(Ordering::Relaxed);
                *p = Point { x: xy as u16, y: (xy >> 16) as u16, id: snapshot.id[i].load(Ordering::Relaxed) };
            }
            fence(Ordering::Acquire);
            if seq % 2 == 0 && snapshot.seq.load(Ordering::Relaxed) == seq {
                return n;
            }
        }
    }

    /// Blocks until the controller signals a sample; without an INT line, until
    /// the next poll.
    pub fn wait_interrupt(&self) {
        if let Some(wait) = self.wait.get() {
            wait.take();
        }
    }

    /// Runs on the dispatch task with every sample, display space, empty on
    /// release. Takes effect from the next sample, so the previous callback may
    /// still run once after this returns.
    pub fn set_event_cb(&self, callback: Option<EventCallback>) {
        *self.callback.lock() = Some(callback);
        self.callback_changed.store(true, Ordering::Release);
        if let Some(n) = self.notifier.get() {
            n.notify();
        }
    }

    fn publish(&self, points: &[Point]) {
        let next = 1 - self.active.load(Ordering::Relaxed);
        let snapshot = &self.snapshots[next];
        snapshot.seq.fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        snapshot.count.store(points.len() as u8, Ordering::Relaxed);
        for (i, p) in points.iter().enumerate() {
            snapshot.xy[i].store(u32::from(p.x) | u32::from(p.y) << 16, Ordering::Relaxed);
            snapshot.id[i].store(p.id, Ordering::Relaxed);
        }
        snapshot.seq.fetch_add(1, Ordering::Release);
        self.active.store(next, Ordering::Release);
    }
}

static TOUCH: OnceLock<Touch> = OnceLock::new();

pub(crate) fn start<D: Driver>(driver: D, config: Config) -> Result<&'static Touch> {
    let touch = TOUCH.get_or_try_init(Touch::new)?;
    touch.wait.get_or_try_init(|| Semaphore::try_new(0, 1))?;
    if let Some(pin) = config.int_pin {
        attach_int(pin, touch).inspect_err(|err| error!(TAG, "INT on GPIO{pin}: {err}"))?;
    }
    let notifier = dispatch::add(Box::new(TouchSource {
        driver,
        touch,
        config,
        callback: None,
        settled: 0,
        down: false,
        failing: false,
    }))?;
    let _ = touch.notifier.set(notifier);
    Ok(touch)
}

#[cfg(not(target_os = "none"))]
fn attach_int(_pin: i32, _touch: &'static Touch) -> Result<()> {
    Err(idf::Error::NOT_SUPPORTED)
}

#[cfg(target_os = "none")]
fn attach_int(pin: i32, touch: &'static Touch) -> Result<()> {
    use idf::gpio;
    gpio::install_isr_service()?;
    gpio::set_interrupt(pin, gpio::Interrupt::FallingEdge)?;
    unsafe { gpio::add_isr_handler(pin, on_int, (touch as *const Touch).cast_mut().cast()) }
}

#[cfg(target_os = "none")]
unsafe extern "C" fn on_int(arg: *mut core::ffi::c_void) {
    let touch = unsafe { &*arg.cast::<Touch>() };
    touch.int_pending.store(true, Ordering::Release);
    if let Some(n) = touch.notifier.get() {
        n.notify_from_isr();
    }
    if let Some(wait) = touch.wait.get() {
        wait.give_from_isr();
    }
}

struct TouchSource<D> {
    driver: D,
    touch: &'static Touch,
    config: Config,
    callback: Option<EventCallback>,
    settled: u8,
    down: bool,
    failing: bool,
}

impl Config {
    fn orient(&self, p: Point) -> Point {
        let (mut x, mut y) = if self.swap_xy { (p.y, p.x) } else { (p.x, p.y) };
        if self.mirror_x && self.width > 0 {
            x = (self.width - 1).saturating_sub(x);
        }
        if self.mirror_y && self.height > 0 {
            y = (self.height - 1).saturating_sub(y);
        }
        Point { x, y, id: p.id }
    }
}

impl<D: Driver> TouchSource<D> {
    fn emit(&mut self, points: &[Point]) {
        self.touch.publish(points);
        if let Some(cb) = &mut self.callback {
            cb(points);
        }
    }
}

impl<D: Driver> Source for TouchSource<D> {
    fn tick(&mut self) -> Next {
        if self.touch.callback_changed.swap(false, Ordering::Acquire) {
            if let Some(cb) = self.touch.callback.lock().take() {
                self.callback = cb;
            }
        }

        let has_int = self.config.int_pin.is_some();
        let int_pending = self.touch.int_pending.swap(false, Ordering::AcqRel);
        if has_int && !int_pending && self.settled >= DEFAULT_SETTLE_COUNT && !self.down {
            return Next::Idle;
        }

        let mut points = [Point::default(); MAX_POINTS];
        let n = match self.driver.poll() {
            Ok(raw) => {
                for (out, p) in points.iter_mut().zip(raw) {
                    *out = self.config.orient(*p);
                }
                raw.len().min(MAX_POINTS)
            }
            Err(err) => {
                if !self.failing {
                    warn!(TAG, "poll: {err}");
                    self.failing = true;
                }
                return Next::After(DEFAULT_POLL_INTERVAL);
            }
        };
        self.failing = false;
        if n > 0 {
            self.emit(&points[..n]);
            self.down = true;
            self.settled = 0;
        } else {
            if self.down {
                self.emit(&[]);
            }
            self.down = false;
            self.settled = self.settled.saturating_add(1);
        }
        if !has_int && let Some(wait) = self.touch.wait.get() {
            wait.give();
        }

        if self.down || self.settled < DEFAULT_SETTLE_COUNT || !has_int {
            Next::After(DEFAULT_POLL_INTERVAL)
        } else {
            Next::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, DEFAULT_SETTLE_COUNT, Driver, Point, Touch, TouchSource};
    use crate::dispatch::{Next, Source};
    use alloc::boxed::Box;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use idf::Result;
    use idf::sync::Mutex;

    struct Fake(Arc<Mutex<Vec<Point>>>, Vec<Point>);

    impl Driver for Fake {
        fn poll(&mut self) -> Result<&[Point]> {
            self.1.clone_from(&self.0.lock());
            Ok(&self.1)
        }
    }

    fn p(x: u16, y: u16) -> Point {
        Point { x, y, id: 0 }
    }

    fn source(config: Config) -> (TouchSource<Fake>, Arc<Mutex<Vec<Point>>>, &'static Touch) {
        let touched = Arc::new(Mutex::new(Vec::new()));
        let touch: &'static Touch = Box::leak(Box::new(Touch::new().unwrap()));
        let src = TouchSource {
            driver: Fake(touched.clone(), Vec::new()),
            touch,
            config,
            callback: None,
            settled: 0,
            down: false,
            failing: false,
        };
        (src, touched, touch)
    }

    fn config(int_pin: Option<i32>) -> Config {
        Config { width: 100, height: 200, swap_xy: false, mirror_x: false, mirror_y: false, int_pin }
    }

    #[test]
    fn read_returns_the_latest_sample() {
        let (_, _, touch) = source(config(None));
        let mut out = [Point::default(); 4];
        assert_eq!(touch.read(&mut out), 0);
        touch.publish(&[p(1, 2), p(3, 4)]);
        assert_eq!(touch.read(&mut out), 2);
        assert_eq!(out[..2], [p(1, 2), p(3, 4)]);
        touch.publish(&[p(5, 6)]);
        assert_eq!(touch.read(&mut out[..1]), 1);
        assert_eq!(out[0], p(5, 6));
        touch.publish(&[p(7, 8), p(9, 10)]);
        assert_eq!(touch.read(&mut out[..1]), 1);
        assert_eq!(out[0], p(7, 8));
    }

    #[test]
    fn int_driven_source_settles_to_idle() {
        let (mut src, touched, touch) = source(config(Some(0)));
        let events: Arc<Mutex<Vec<Vec<Point>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        src.callback = Some(Box::new(move |pts| sink.lock().push(pts.to_vec())));

        for _ in 0..DEFAULT_SETTLE_COUNT - 1 {
            assert!(matches!(src.tick(), Next::After(_)));
        }
        assert!(matches!(src.tick(), Next::Idle));
        assert!(matches!(src.tick(), Next::Idle));
        assert!(events.lock().is_empty());

        *touched.lock() = alloc::vec![p(10, 20)];
        touch.int_pending.store(true, core::sync::atomic::Ordering::Release);
        assert!(matches!(src.tick(), Next::After(_)));
        assert!(matches!(src.tick(), Next::After(_)));
        touched.lock().clear();
        for _ in 0..DEFAULT_SETTLE_COUNT - 1 {
            assert!(matches!(src.tick(), Next::After(_)));
        }
        assert!(matches!(src.tick(), Next::Idle));

        let events = events.lock();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], [p(10, 20)]);
        assert_eq!(events[1], [p(10, 20)]);
        assert!(events[2].is_empty());
        let mut out = [Point::default(); 2];
        assert_eq!(touch.read(&mut out), 0);
    }

    #[test]
    fn polling_source_never_idles() {
        let (mut src, _, _) = source(config(None));
        for _ in 0..DEFAULT_SETTLE_COUNT * 2 {
            assert!(matches!(src.tick(), Next::After(_)));
        }
    }

    #[test]
    fn orientation_maps_into_display_space() {
        let (mut src, touched, touch) = source(Config { swap_xy: true, mirror_x: true, mirror_y: true, ..config(None) });
        *touched.lock() = alloc::vec![p(10, 20)];
        src.tick();
        let mut out = [Point::default(); 1];
        assert_eq!(touch.read(&mut out), 1);
        assert_eq!(out[0], p(100 - 1 - 20, 200 - 1 - 10));
    }

    #[test]
    fn callback_is_replaced_on_the_next_tick() {
        let (mut src, touched, touch) = source(config(None));
        let count = Arc::new(Mutex::new(0));
        let c = count.clone();
        touch.set_event_cb(Some(Box::new(move |_| *c.lock() += 1)));
        *touched.lock() = alloc::vec![p(1, 1)];
        src.tick();
        assert_eq!(*count.lock(), 1);
        touch.set_event_cb(None);
        src.tick();
        assert_eq!(*count.lock(), 1);
    }
}
