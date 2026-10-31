/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::ffi::CStr;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::time::Duration;

use bsp_rs::display::{Display, Rotation};
use bsp_rs::touch::{Point, Touch};
use devkit_gfx::{DisplayList, GlyphCache};
use idf::heap::Caps;
use idf::log::error;
use idf::sync::{Mutex, Receiver, Semaphore, Sender, channel};
use idf::thread;
use idf::time::Instant;

use crate::component::{Component, Screen};
use crate::erased::Erased;
use crate::keyboard::KeyboardMode;
use crate::paint::Effects;
use crate::presenter::{Config, Presenter, Strips};
use crate::style::Insets;
use crate::text_buffer::TextInput;
use crate::theme::Theme;
use crate::tree::Tree;

const TAG: &CStr = c"ui";
const QUEUE_DEPTH: usize = 32;
const FRAME: Duration = Duration::from_millis(16);
const IDLE_WAIT: Duration = Duration::from_secs(3600);
const CARET_WAIT: Duration = Duration::from_millis(250);
const REPEAT_WAIT: Duration = Duration::from_millis(20);
const TAP_HOLD: Duration = Duration::from_millis(40);
const ROOT: u16 = 0;
const DROP_LOG_EVERY: u32 = 256;

pub struct AppConfig {
    pub theme: &'static Theme,
    pub presenter: Config,
    pub glyph_cache_bytes: usize,
    pub glyph_cache_caps: Caps,
    pub keyboard: KeyboardMode,
}

impl AppConfig {
    pub fn new(theme: &'static Theme) -> Self {
        Self { theme, presenter: Config::default(), glyph_cache_bytes: 128 * 1024, glyph_cache_caps: Caps::SPIRAM, keyboard: KeyboardMode::Auto }
    }
}

enum Question {
    Stats,
    Tree,
    TapPoint(String),
    Invoke(String),
}

struct Exchange {
    question: Question,
    answer: Mutex<Result<String, String>>,
    done: Semaphore,
}

struct StripSwap {
    strips: Mutex<Option<Result<Strips, Strips>>>,
    lanes: usize,
    done: Semaphore,
}

enum Event {
    Input(TextInput),
    KeyboardConnected(bool),
    SetKeyboardMode(KeyboardMode),
    SetStrips(Arc<StripSwap>),
    Touch(Option<(u16, u16)>, u32),
    Post(u16, Erased),
    Ask(Arc<Exchange>),
}

struct Shared {
    events: Sender<Event>,
    pending: AtomicU32,
    dropped: AtomicU32,
    busy: AtomicBool,
    display: &'static Display,
    rotation: Rotation,
    touch: Option<&'static Touch>,
}

impl Shared {
    fn send(&self, event: Event) -> bool {
        self.pending.fetch_add(1, Ordering::AcqRel);
        if self.events.try_send(event).is_err() {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    fn to_logical(&self, x: u16, y: u16) -> (i16, i16) {
        let size = self.display.size();
        let (x, y, w, h) = (x as i16, y as i16, size.width as i16, size.height as i16);
        match self.rotation {
            Rotation::R0 => (x, y),
            Rotation::R90 => (h - 1 - y, x),
            Rotation::R180 => (w - 1 - x, h - 1 - y),
            Rotation::R270 => (y, w - 1 - x),
        }
    }

    fn to_panel(&self, x: i16, y: i16) -> (u16, u16) {
        let size = self.display.size();
        let (w, h) = (size.width as i16, size.height as i16);
        let (px, py) = match self.rotation {
            Rotation::R0 => (x, y),
            Rotation::R90 => (y, h - 1 - x),
            Rotation::R180 => (w - 1 - x, h - 1 - y),
            Rotation::R270 => (w - 1 - y, x),
        };
        (px as u16, py as u16)
    }
}

/// Posts messages to a component from any task.
pub struct Handle<M> {
    shared: Arc<Shared>,
    slot: u16,
    _msg: PhantomData<fn(M)>,
}

impl<M> Clone for Handle<M> {
    fn clone(&self) -> Self {
        Self { shared: self.shared.clone(), slot: self.slot, _msg: PhantomData }
    }
}

impl<M: Copy> Handle<M> {
    /// `false` when the queue is full.
    pub fn post(&self, msg: M) -> bool {
        self.shared.send(Event::Post(self.slot, Erased::new(msg)))
    }

    /// `Presenter::set_strips` between frames. Blocks for the running frame, so
    /// it cannot be called from the UI task; on `Err` the strips come back.
    pub fn set_strips(&self, strips: Strips, lanes: usize) -> Result<Strips, Strips> {
        let swap = Arc::new(StripSwap { strips: Mutex::new(Some(Ok(strips))), lanes, done: Semaphore::new(0, 1) });
        while !self.shared.send(Event::SetStrips(swap.clone())) {
            thread::sleep(FRAME);
        }
        swap.done.take();
        swap.strips.lock().take().expect("answered")
    }
}

/// Feeds text input from another task, as a physical keyboard does.
#[derive(Clone)]
pub struct Input {
    shared: Arc<Shared>,
}

impl Input {
    /// To the focused input. `false` when the queue is full.
    pub fn send(&self, input: TextInput) -> bool {
        self.shared.send(Event::Input(input))
    }

    /// With `KeyboardMode::Auto` the on-screen keyboard steps aside while a
    /// physical one is connected.
    pub fn set_keyboard_connected(&self, connected: bool) -> bool {
        self.shared.send(Event::KeyboardConnected(connected))
    }

    pub fn set_keyboard_mode(&self, mode: KeyboardMode) -> bool {
        self.shared.send(Event::SetKeyboardMode(mode))
    }
}

/// Inspects and drives the UI from another task, for test harnesses.
#[derive(Clone)]
pub struct Probe {
    shared: Arc<Shared>,
}

impl Probe {
    fn ask(&self, question: Question) -> Result<String, String> {
        let exchange = Arc::new(Exchange {
            question,
            answer: Mutex::new(Err("no answer".to_string())),
            done: Semaphore::new(0, 1),
        });
        if !self.shared.send(Event::Ask(exchange.clone())) {
            return Err("queue full".to_string());
        }
        exchange.done.take();
        let answer = exchange.answer.lock().clone();
        answer
    }

    pub fn idle(&self) -> bool {
        self.shared.pending.load(Ordering::Acquire) == 0 && !self.shared.busy.load(Ordering::Acquire)
    }

    /// `id role x y w h "text"`, entries separated by `; `.
    pub fn tree(&self) -> Result<String, String> {
        self.ask(Question::Tree)
    }

    /// Slowest and last times of the frame stages, in microseconds, and the
    /// node count.
    pub fn stats(&self) -> Result<String, String> {
        self.ask(Question::Stats)
    }

    /// Presses and releases the node through the touch path.
    pub fn tap(&self, id: &str) -> Result<(), String> {
        let answer = self.ask(Question::TapPoint(id.to_string()))?;
        let (x, y) = answer.split_once(' ').ok_or("bad point")?;
        let x: i16 = x.parse().map_err(|_| "bad point")?;
        let y: i16 = y.parse().map_err(|_| "bad point")?;
        let touch = self.shared.touch.ok_or("no touch")?;
        let (px, py) = self.shared.to_panel(x, y);
        touch.inject(&[Point { x: px, y: py, id: 0 }]);
        thread::sleep(TAP_HOLD);
        touch.inject(&[]);
        Ok(())
    }

    /// Fires the node's handler directly.
    pub fn invoke(&self, id: &str) -> Result<(), String> {
        self.ask(Question::Invoke(id.to_string())).map(|_| ())
    }
}

#[derive(Default)]
struct Timing {
    frames: u32,
    update: (u32, u32),
    paint: (u32, u32),
    render: (u32, u32),
}

impl Timing {
    fn describe(&self, nodes: usize) -> String {
        let (update, paint, render) = (self.update, self.paint, self.render);
        alloc::format!(
            "frames={} nodes={nodes} update={}/{} paint={}/{} render={}/{}",
            self.frames, update.0, update.1, paint.0, paint.1, render.0, render.1
        )
    }
}

fn note(stage: &mut (u32, u32), from: Instant) -> Instant {
    let now = Instant::now();
    let micros = now.duration_since(from).as_micros() as u32;
    *stage = (micros, stage.1.max(micros));
    now
}

/// Runs a component tree on the display.
pub struct App<C: Screen> {
    shared: Arc<Shared>,
    receiver: Receiver<Event>,
    tree: Tree,
    presenter: Presenter,
    cache: GlyphCache<Caps>,
    effects: Effects,
    data_caps: Caps,
    timing: Timing,
    _root: PhantomData<fn(C)>,
}

impl<C: Screen> App<C> {
    pub fn new(display: &'static Display, touch: Option<&'static Touch>, root: C, config: &AppConfig) -> idf::Result<Self> {
        let presenter = Presenter::new(display, &config.presenter)?;
        let (events, receiver) = channel(QUEUE_DEPTH);
        let shared = Arc::new(Shared {
            events,
            pending: AtomicU32::new(0),
            dropped: AtomicU32::new(0),
            busy: AtomicBool::new(true),
            display,
            rotation: config.presenter.rotation,
            touch,
        });
        if let Some(touch) = touch {
            let shared = shared.clone();
            touch.set_event_cb(Some(Box::new(move |points: &[Point]| {
                let first = points.iter().min_by_key(|p| p.id).map(|p| (p.x, p.y));
                let now = Instant::now().since_boot().as_millis() as u32;
                if !shared.send(Event::Touch(first, now)) {
                    let dropped = shared.dropped.fetch_add(1, Ordering::Relaxed);
                    if dropped % DROP_LOG_EVERY == 0 {
                        error!(TAG, "input queue full, {} samples dropped", dropped + 1);
                    }
                }
            })));
        }
        let area = presenter.area();
        Ok(Self {
            shared,
            receiver,
            tree: {
                let mut tree = Tree::new(root, area, config.presenter.data_caps, config.theme);
                tree.add_keyboard(config.keyboard);
                tree
            },
            presenter,
            cache: GlyphCache::new_in(config.glyph_cache_bytes, config.glyph_cache_caps),
            effects: Effects::new(config.presenter.data_caps),
            data_caps: config.presenter.data_caps,
            timing: Timing::default(),
            _root: PhantomData,
        })
    }

    /// Adds a component above the screens that stays across navigation, and
    /// the space it covers.
    pub fn chrome<B: Component>(&mut self, component: B, insets: Insets) -> Handle<B::Msg> {
        let slot = self.tree.add_chrome(component, insets);
        Handle { shared: self.shared.clone(), slot, _msg: PhantomData }
    }

    /// `Presenter::set_strips` before `run`.
    pub fn set_strips(&mut self, strips: Strips, lanes: usize) -> Result<Strips, Strips> {
        self.presenter.set_strips(strips, lanes)
    }

    pub fn handle(&self) -> Handle<C::Msg> {
        Handle { shared: self.shared.clone(), slot: ROOT, _msg: PhantomData }
    }

    pub fn input(&self) -> Input {
        Input { shared: self.shared.clone() }
    }

    pub fn probe(&self) -> Probe {
        Probe { shared: self.shared.clone() }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Input(input) => self.tree.input(input),
            Event::KeyboardConnected(connected) => self.tree.set_keyboard_connected(connected),
            Event::SetKeyboardMode(mode) => self.tree.set_keyboard_mode(mode),
            Event::SetStrips(swap) => {
                let mut slot = swap.strips.lock();
                if let Some(Ok(strips)) = slot.take() {
                    *slot = Some(self.presenter.set_strips(strips, swap.lanes));
                }
                drop(slot);
                swap.done.give();
            }
            Event::Touch(point, sampled_ms) => {
                let logical = point.map(|(x, y)| self.shared.to_logical(x, y));
                self.tree.touch(logical, sampled_ms);
            }
            Event::Post(slot, msg) => self.tree.post(slot, msg),
            Event::Ask(exchange) => {
                self.tree.update();
                let answer = match &exchange.question {
                    Question::Stats => Ok(self.timing.describe(self.tree.nodes.len())),
                    Question::Tree => Ok(self.tree.describe()),
                    Question::TapPoint(id) => self.tree.tap_point(id).map(|(x, y)| alloc::format!("{x} {y}")),
                    Question::Invoke(id) => self.tree.invoke(id).map(|()| String::new()),
                };
                *exchange.answer.lock() = answer;
                exchange.done.give();
            }
        }
        self.shared.pending.fetch_sub(1, Ordering::AcqRel);
    }

    fn drain(&mut self) {
        while let Some(event) = self.receiver.try_recv() {
            self.handle_event(event);
        }
    }

    fn apply_render_mode(&mut self) {
        let Some((slot, request)) = self.tree.render_request.take() else { return };
        let given_up = match self.presenter.set_strips(request.strips, request.lanes) {
            Ok(old) => old,
            Err(same) => same,
        };
        self.tree.render_done(slot, request.reply, given_up);
    }

    fn frame(&mut self) {
        let start = Instant::now();
        self.tree.update();
        self.apply_render_mode();
        let mut update = (0, 0);
        let updated = note(&mut update, start);
        let damage = self.tree.take_damage();
        if damage.is_empty() && !self.presenter.has_new_damage() {
            return;
        }
        self.timing.frames += 1;
        self.timing.update = (update.0, self.timing.update.1.max(update.0));
        for rect in damage {
            self.presenter.invalidate(rect);
        }
        let mut list = DisplayList::new_in(self.data_caps);
        self.tree.paint(&mut self.effects, &mut list);
        let painted = note(&mut self.timing.paint, updated);
        match self.presenter.render(&list, &mut self.cache) {
            Ok(stats) if stats.glyphs_missing > 0 => {
                error!(TAG, "{} glyphs did not fit the cache", stats.glyphs_missing);
            }
            Ok(_) => {}
            Err(err) => error!(TAG, "render: {err}"),
        }
        note(&mut self.timing.render, painted);
    }

    /// Runs on the calling task, which then owns the UI.
    pub fn run(mut self) -> ! {
        let mut last = Instant::now();
        loop {
            self.frame();
            self.shared.busy.store(!self.tree.idle(), Ordering::Release);
            // While animating the presenter paces frames; a timed wait here
            // would add at least two ticks to each.
            let wait = if self.tree.repeating() {
                REPEAT_WAIT
            } else if self.tree.has_caret() {
                CARET_WAIT
            } else {
                IDLE_WAIT
            };
            if !self.tree.animating()
                && let Some(event) = self.receiver.recv_timeout(wait)
            {
                self.shared.busy.store(true, Ordering::Release);
                self.handle_event(event);
            }
            if let Err(err) = self.presenter.acquire() {
                error!(TAG, "acquire: {err}");
            }
            self.drain();
            let now = Instant::now();
            self.tree.tick(now.duration_since(last).as_millis() as u32);
            last = now;
        }
    }
}
