/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering, fence};
use core::time::Duration;

use idf::log::{error, warn};
use idf::sync::{Mutex, OnceLock, Semaphore};
use devkit_gfx::Rotation;
use idf::Result;

use crate::dispatch::{self, Next, Notifier, Source};

const TAG: &CStr = c"touch";

pub const MAX_POINTS: usize = 10;
const DEFAULT_SETTLE_COUNT: u8 = 5;
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const TRACK_REACH: i32 = 200;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Point {
    pub x: u16,
    pub y: u16,
    /// Track id from the controller.
    pub id: u8,
}

pub type EventCallback = Box<dyn FnMut(&[Point]) + Send>;

/// Gives a chip's untracked points ids: each takes the id of the nearest point
/// of the last sample within reach, or a new one.
#[allow(dead_code)]
pub(crate) fn assign_tracks(last: &[Point], next: &mut [Point], next_track: &mut u8) {
    let distance = |a: &Point, b: &Point| {
        let (dx, dy) = (i32::from(a.x) - i32::from(b.x), i32::from(a.y) - i32::from(b.y));
        dx * dx + dy * dy
    };
    let mut taken = [false; MAX_POINTS];
    let mut matched = [false; MAX_POINTS];
    for (point, matched) in next.iter_mut().zip(&mut matched) {
        let nearest = (0..last.len()).filter(|&i| !taken[i]).min_by_key(|&i| distance(point, &last[i]));
        if let Some(i) = nearest.filter(|&i| distance(point, &last[i]) <= TRACK_REACH * TRACK_REACH) {
            taken[i] = true;
            point.id = last[i].id;
            *matched = true;
        }
    }
    for i in 0..next.len() {
        if !matched[i] {
            // Past every id of the last sample, so a new finger is never read
            // as a lifted one that jumped.
            while last.iter().any(|p| p.id == *next_track) {
                *next_track = next_track.wrapping_add(1);
            }
            next[i].id = *next_track;
            *next_track = next_track.wrapping_add(1);
        }
    }
}

pub(crate) trait Driver: Send + 'static {
    /// The latest known touches in chip space. A chip with a data-ready
    /// handshake returns its last frame when nothing new arrived.
    fn poll(&mut self) -> Result<&[Point]>;
}

pub(crate) struct Config {
    /// The panel's unturned picture, which `swap_xy` and the mirrors map chip
    /// space onto.
    pub width: u16,
    pub height: u16,
    pub swap_xy: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub rotation: Rotation,
    pub int: Int,
    /// Areas of chip space that are buttons rather than touches; `ZoneButtons`
    /// reports them.
    pub zones: &'static [Zone],
}

/// `x0..x1` by `y0..y1` in chip space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Zone {
    pub x0: u16,
    pub y0: u16,
    pub x1: u16,
    pub y1: u16,
}

impl Zone {
    fn contains(&self, p: &Point) -> bool {
        (self.x0..self.x1).contains(&p.x) && (self.y0..self.y1).contains(&p.y)
    }
}

/// Swaps the axes, then mirrors each about the size it maps onto.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Transform {
    swap: bool,
    mirror_x: bool,
    mirror_y: bool,
    width: u16,
    height: u16,
}

impl Transform {
    /// From the stored picture of `width` x `height` to the logical one.
    fn turn(rotation: Rotation, width: u16, height: u16) -> Self {
        let (swap, mirror_x, mirror_y) = match rotation {
            Rotation::R0 => (false, false, false),
            Rotation::R90 => (true, true, false),
            Rotation::R180 => (false, true, true),
            Rotation::R270 => (true, false, true),
        };
        let (width, height) = if swap { (height, width) } else { (width, height) };
        Self { swap, mirror_x, mirror_y, width, height }
    }

    fn then(self, next: Self) -> Self {
        let (inner_x, inner_y) = if next.swap { (self.mirror_y, self.mirror_x) } else { (self.mirror_x, self.mirror_y) };
        Self {
            swap: self.swap != next.swap,
            mirror_x: next.mirror_x != inner_x,
            mirror_y: next.mirror_y != inner_y,
            width: next.width,
            height: next.height,
        }
    }

    fn apply(&self, p: Point) -> Point {
        let (mut x, mut y) = if self.swap { (p.y, p.x) } else { (p.x, p.y) };
        if self.mirror_x && self.width > 0 {
            x = (self.width - 1).saturating_sub(x);
        }
        if self.mirror_y && self.height > 0 {
            y = (self.height - 1).saturating_sub(y);
        }
        Point { x, y, id: p.id }
    }
}

const ROTATIONS: [Rotation; 4] = [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270];

fn rotation_code(rotation: Rotation) -> u8 {
    ROTATIONS.iter().position(|r| *r == rotation).unwrap_or(0) as u8
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) enum Int {
    /// Polled continuously.
    None,
    /// A falling edge marks a new sample.
    Gpio(i32),
    /// The driver calls `Touch::interrupt` on a new sample.
    External,
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
    injected: Mutex<Option<Vec<Point>>>,
    has_injected: AtomicBool,
    rotation: AtomicU8,
    notifier: OnceLock<Notifier>,
    zones: AtomicU8,
    zones_pressed: AtomicU8,
}

impl Touch {
    fn new() -> Result<Self> {
        Ok(Self {
            snapshots: [Snapshot::new(), Snapshot::new()],
            active: AtomicUsize::new(0),
            wait: OnceLock::new(),
            int_pending: AtomicBool::new(false),
            callback: Mutex::new(None),
            callback_changed: AtomicBool::new(false),
            injected: Mutex::new(None),
            has_injected: AtomicBool::new(false),
            rotation: AtomicU8::new(0),
            notifier: OnceLock::new(),
            zones: AtomicU8::new(0),
            zones_pressed: AtomicU8::new(0),
        })
    }

    /// Reports points turned as a display by `rotation` shows its picture,
    /// from the next sample on.
    pub fn set_rotation(&self, rotation: Rotation) {
        self.rotation.store(rotation_code(rotation), Ordering::Release);
    }

    pub fn rotation(&self) -> Rotation {
        ROTATIONS[usize::from(self.rotation.load(Ordering::Acquire))]
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

    /// Delivers `points`, in display space, as a sample from the controller;
    /// an empty slice is a release. Runs through the dispatch task.
    pub fn inject(&self, points: &[Point]) {
        *self.injected.lock() = Some(points.iter().take(MAX_POINTS).copied().collect());
        self.has_injected.store(true, Ordering::Release);
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
    if let Int::Gpio(pin) = config.int {
        attach_int(pin, touch).inspect_err(|err| error!(TAG, "INT on GPIO{pin}: {err}"))?;
    }
    touch.set_rotation(config.rotation);
    touch.zones.store(config.zones.len().min(crate::button::MAX_BUTTONS) as u8, Ordering::Release);
    let notifier = dispatch::add(Box::new(TouchSource::new(driver, touch, &config)))?;
    let _ = touch.notifier.set(notifier);
    Ok(touch)
}

/// The zones of the touch that started, as buttons in their order.
pub(crate) struct ZoneButtons;

impl crate::button::Driver for ZoneButtons {
    fn count(&self) -> usize {
        TOUCH.get().map_or(0, |t| usize::from(t.zones.load(Ordering::Acquire)))
    }

    fn sample(&mut self, pressed: &mut [bool]) -> Result<()> {
        let bits = TOUCH.get().map_or(0, |t| t.zones_pressed.load(Ordering::Acquire));
        for (i, p) in pressed.iter_mut().enumerate() {
            *p = bits & 1 << i != 0;
        }
        Ok(())
    }

    fn notifies(&self) -> bool {
        true
    }
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

impl Touch {
    #[cfg_attr(target_os = "none", allow(dead_code))]
    pub(crate) fn interrupt(&self) {
        self.int_pending.store(true, Ordering::Release);
        if let Some(n) = self.notifier.get() {
            n.notify();
        }
        if let Some(wait) = self.wait.get() {
            wait.give();
        }
    }
}

struct TouchSource<D> {
    driver: D,
    touch: &'static Touch,
    int: Int,
    mount: Transform,
    rotation: Rotation,
    transform: Transform,
    callback: Option<EventCallback>,
    zones: &'static [Zone],
    settled: u8,
    down: bool,
    failing: bool,
}

impl<D: Driver> TouchSource<D> {
    fn new(driver: D, touch: &'static Touch, config: &Config) -> Self {
        let mount = Transform {
            swap: config.swap_xy,
            mirror_x: config.mirror_x,
            mirror_y: config.mirror_y,
            width: config.width,
            height: config.height,
        };
        let rotation = touch.rotation();
        Self {
            driver,
            touch,
            int: config.int,
            mount,
            rotation,
            transform: mount.then(Transform::turn(rotation, config.width, config.height)),
            callback: None,
            zones: config.zones,
            settled: 0,
            down: false,
            failing: false,
        }
    }

    /// Notes which zones `raw` presses and drops the points inside them.
    fn split_zones(&self, raw: &[Point], out: &mut [Point; MAX_POINTS]) -> usize {
        let mut pressed = 0u8;
        let mut n = 0;
        for p in raw.iter().take(MAX_POINTS) {
            match self.zones.iter().position(|z| z.contains(p)) {
                Some(i) => pressed |= 1 << i,
                None => {
                    out[n] = self.transform.apply(*p);
                    n += 1;
                }
            }
        }
        if self.touch.zones_pressed.swap(pressed, Ordering::AcqRel) != pressed {
            crate::button::notify();
        }
        n
    }

    fn follow_rotation(&mut self) {
        let rotation = self.touch.rotation();
        if rotation != self.rotation {
            self.rotation = rotation;
            self.transform = self.mount.then(Transform::turn(rotation, self.mount.width, self.mount.height));
        }
    }

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

        if self.touch.has_injected.swap(false, Ordering::Acquire) {
            if let Some(points) = self.touch.injected.lock().take() {
                self.emit(&points);
            }
        }

        let has_int = !matches!(self.int, Int::None);
        let int_pending = self.touch.int_pending.swap(false, Ordering::AcqRel);
        if has_int && !int_pending && self.settled >= DEFAULT_SETTLE_COUNT && !self.down {
            return Next::Idle;
        }

        self.follow_rotation();
        let mut points = [Point::default(); MAX_POINTS];
        let polled = self.driver.poll().map(|raw| {
            let mut copy = [Point::default(); MAX_POINTS];
            let n = raw.len().min(MAX_POINTS);
            copy[..n].copy_from_slice(&raw[..n]);
            (copy, n)
        });
        let n = match polled {
            Ok((raw, count)) => self.split_zones(&raw[..count], &mut points),
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
    use super::{Config, DEFAULT_SETTLE_COUNT, Driver, Int, Point, ROTATIONS, Touch, TouchSource, Transform, Zone, assign_tracks};
    use devkit_gfx::Rotation;
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
        touch.set_rotation(config.rotation);
        let src = TouchSource::new(Fake(touched.clone(), Vec::new()), touch, &config);
        (src, touched, touch)
    }

    fn config(int: Int) -> Config {
        Config { width: 100, height: 200, swap_xy: false, mirror_x: false, mirror_y: false, rotation: Rotation::R0, int, zones: &[] }
    }

    #[test]
    fn points_in_a_zone_press_its_button_instead_of_touching() {
        static ZONES: [Zone; 2] = [Zone { x0: 0, y0: 200, x1: 50, y1: 240 }, Zone { x0: 50, y0: 200, x1: 100, y1: 240 }];
        let (mut src, touched, touch) = source(Config { zones: &ZONES, ..config(Int::None) });
        *touched.lock() = alloc::vec![p(10, 20), p(60, 210)];
        src.tick();
        let mut out = [Point::default(); 4];
        assert_eq!(touch.read(&mut out), 1);
        assert_eq!(out[0], p(10, 20));
        assert_eq!(touch.zones_pressed.load(core::sync::atomic::Ordering::Acquire), 0b10);
        touched.lock().clear();
        src.tick();
        assert_eq!(touch.zones_pressed.load(core::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn read_returns_the_latest_sample() {
        let (_, _, touch) = source(config(Int::None));
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
        let (mut src, touched, touch) = source(config(Int::External));
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
    fn injected_points_reach_readers_and_the_callback() {
        let (mut src, _, touch) = source(config(Int::External));
        let count = Arc::new(Mutex::new(0));
        let c = count.clone();
        src.callback = Some(Box::new(move |_| *c.lock() += 1));
        touch.inject(&[p(5, 6)]);
        src.tick();
        let mut out = [Point::default(); 2];
        assert_eq!(touch.read(&mut out), 1);
        assert_eq!(out[0], p(5, 6));
        touch.inject(&[]);
        src.tick();
        assert_eq!(touch.read(&mut out), 0);
        assert_eq!(*count.lock(), 2);
    }

    #[test]
    fn polling_source_never_idles() {
        let (mut src, _, _) = source(config(Int::None));
        for _ in 0..DEFAULT_SETTLE_COUNT * 2 {
            assert!(matches!(src.tick(), Next::After(_)));
        }
    }

    #[test]
    fn orientation_maps_into_display_space() {
        let (mut src, touched, touch) = source(Config { swap_xy: true, mirror_x: true, mirror_y: true, ..config(Int::None) });
        *touched.lock() = alloc::vec![p(10, 20)];
        src.tick();
        let mut out = [Point::default(); 1];
        assert_eq!(touch.read(&mut out), 1);
        assert_eq!(out[0], p(100 - 1 - 20, 200 - 1 - 10));
    }

    #[test]
    fn a_rotation_turns_points_into_the_logical_picture() {
        let (mut src, touched, touch) = source(config(Int::None));
        *touched.lock() = alloc::vec![p(10, 20)];
        let mut out = [Point::default(); 1];
        for (rotation, expected) in [
            (Rotation::R90, p(200 - 1 - 20, 10)),
            (Rotation::R180, p(100 - 1 - 10, 200 - 1 - 20)),
            (Rotation::R270, p(20, 100 - 1 - 10)),
            (Rotation::R0, p(10, 20)),
        ] {
            touch.set_rotation(rotation);
            src.tick();
            touch.read(&mut out);
            assert_eq!(out[0], expected, "{rotation:?}");
        }
    }

    #[test]
    fn the_composed_transform_matches_the_mount_then_the_turn() {
        let (w, h) = (100, 200);
        for mount in 0..8 {
            let mount = Transform { swap: mount & 1 != 0, mirror_x: mount & 2 != 0, mirror_y: mount & 4 != 0, width: w, height: h };
            let (cw, ch) = if mount.swap { (h, w) } else { (w, h) };
            for rotation in ROTATIONS {
                let turn = Transform::turn(rotation, w, h);
                let composed = mount.then(turn);
                for (x, y) in [(0, 0), (cw - 1, 0), (0, ch - 1), (cw - 1, ch - 1), (7, 31)] {
                    let chip = p(x, y);
                    assert_eq!(composed.apply(chip), turn.apply(mount.apply(chip)), "{mount:?} {rotation:?} {chip:?}");
                }
            }
        }
    }

    #[test]
    fn callback_is_replaced_on_the_next_tick() {
        let (mut src, touched, touch) = source(config(Int::None));
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

    fn at(x: u16, y: u16) -> Point {
        Point { x, y, id: 0 }
    }

    #[test]
    fn a_finger_keeps_its_id_when_the_one_before_it_lifts() {
        let mut track = 0;
        let mut first = [at(100, 100), at(600, 100)];
        assign_tracks(&[], &mut first, &mut track);
        assert_ne!(first[0].id, first[1].id);
        let mut second = [at(605, 110)];
        assign_tracks(&first, &mut second, &mut track);
        assert_eq!(second[0].id, first[1].id);
    }

    #[test]
    fn a_far_landing_in_the_sample_of_a_lift_is_a_new_finger() {
        let mut track = 0;
        let mut first = [at(100, 100)];
        assign_tracks(&[], &mut first, &mut track);
        let mut second = [at(600, 900)];
        assign_tracks(&first, &mut second, &mut track);
        assert_ne!(second[0].id, first[0].id);
    }

    #[test]
    fn new_ids_avoid_those_still_held() {
        let mut track = 255;
        let mut first = [at(100, 100), at(300, 100)];
        assign_tracks(&[], &mut first, &mut track);
        track = first[0].id;
        let mut second = [at(100, 100), at(300, 100), at(700, 900)];
        assign_tracks(&first, &mut second, &mut track);
        assert_ne!(second[2].id, first[0].id);
        assert_ne!(second[2].id, first[1].id);
    }
}
