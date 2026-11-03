/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use core::time::Duration;

use bsp_rs::display::{Display, PixelFormat, Rotation};
use bsp_rs::touch::{MAX_POINTS, Point, Touch};
use devkit_gfx::{DisplayList, GlyphCache, ImageFormat, Rect};
use idf::heap::Caps;
use idf::log::error;
use idf::sync::{Mutex, OnceLock, Receiver, Semaphore, Sender, channel};
use idf::thread;
use idf::time::Instant;

use crate::component::{Component, Screen};
use crate::erased::Erased;
use crate::external::{ExternalSurface, Policy};
use crate::keyboard::KeyboardMode;
use crate::plugin::{Plugin, PluginContext, PluginInit, Store, UiEvent};
use crate::paint::{Effects, Engine};
use crate::presenter::{Config, Presenter, ROW_GRID, Strips};
use crate::text_buffer::TextInput;
use crate::theme::{ColorMode, Theme};
use crate::tree::Tree;

const TAG: &CStr = c"ui";
const QUEUE_DEPTH: usize = 32;
const FRAME: Duration = Duration::from_millis(16);
const IDLE_WAIT: Duration = Duration::from_secs(3600);
const CARET_WAIT: Duration = Duration::from_millis(250);
const REPEAT_WAIT: Duration = Duration::from_millis(20);
const HOLE_WAIT: Duration = Duration::from_millis(100);
const TAP_HOLD: Duration = Duration::from_millis(40);
const DROP_LOG_EVERY: u32 = 256;

pub struct AppConfig {
    pub theme: &'static Theme,
    pub presenter: Config,
    pub glyph_cache_bytes: usize,
    pub glyph_cache_caps: Caps,
    pub keyboard: KeyboardMode,
    pub plugins: Vec<Box<dyn Plugin>>,
    /// Where `Store` keeps its keys.
    pub nvs_namespace: &'static CStr,
}

impl AppConfig {
    pub fn new(theme: &'static Theme) -> Self {
        Self { theme, presenter: Config::default(), glyph_cache_bytes: 128 * 1024, glyph_cache_caps: Caps::SPIRAM, keyboard: KeyboardMode::Auto, plugins: Vec::new(), nvs_namespace: c"devkit_ui" }
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
    Wake,
    SetRotation(Rotation),
    SetColorMode(ColorMode),
    Input(TextInput),
    KeyboardConnected(bool),
    SetKeyboardMode(KeyboardMode),
    SetStrips(Arc<StripSwap>),
    Touch([Point; MAX_POINTS], u8, u32),
    Post(u32, Erased),
    Ask(Arc<Exchange>),
}

/// Filled by `App::new`; handles can exist before that, for a first screen.
pub(crate) type Link = Arc<OnceLock<Arc<Shared>>>;

pub(crate) struct Shared {
    events: Sender<Event>,
    pending: AtomicU32,
    dropped: AtomicU32,
    busy: AtomicBool,
    display: &'static Display,
    rotation: AtomicU8,
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
        match ROTATIONS[usize::from(self.rotation.load(Ordering::Acquire))] {
            Rotation::R0 => (x, y),
            Rotation::R90 => (h - 1 - y, x),
            Rotation::R180 => (w - 1 - x, h - 1 - y),
            Rotation::R270 => (y, w - 1 - x),
        }
    }

    fn to_panel(&self, x: i16, y: i16) -> (u16, u16) {
        let size = self.display.size();
        let (w, h) = (size.width as i16, size.height as i16);
        let (px, py) = match ROTATIONS[usize::from(self.rotation.load(Ordering::Acquire))] {
            Rotation::R0 => (x, y),
            Rotation::R90 => (y, h - 1 - x),
            Rotation::R180 => (w - 1 - x, h - 1 - y),
            Rotation::R270 => (w - 1 - y, x),
        };
        (px as u16, py as u16)
    }
}

pub(crate) fn post(link: &Link, id: u32, msg: Erased) -> bool {
    link.get().is_some_and(|shared| shared.send(Event::Post(id, msg)))
}

/// Posts messages to a component from any task.
pub struct Handle<M> {
    link: Link,
    id: u32,
    _msg: PhantomData<fn(M)>,
}

impl<M> Clone for Handle<M> {
    fn clone(&self) -> Self {
        Self { link: self.link.clone(), id: self.id, _msg: PhantomData }
    }
}

impl<M> Handle<M> {
    pub(crate) fn new(link: Link, id: u32) -> Self {
        Self { link, id, _msg: PhantomData }
    }

    pub(crate) fn id(&self) -> u32 {
        self.id
    }
}

impl<M: Copy> Handle<M> {
    /// `false` when the queue is full, or before `App::new` has returned. A
    /// message for a component that has gone is dropped.
    pub fn post(&self, msg: M) -> bool {
        post(&self.link, self.id, Erased::new(msg))
    }

    /// `Presenter::set_strips` between frames. Blocks for the running frame, so
    /// it cannot be called from the UI task; on `Err` the strips come back.
    pub fn set_strips(&self, strips: Strips, lanes: usize) -> Result<Strips, Strips> {
        let Some(shared) = self.link.get() else { return Err(strips) };
        let swap = Arc::new(StripSwap { strips: Mutex::new(Some(Ok(strips))), lanes, done: Semaphore::new(0, 1) });
        while !shared.send(Event::SetStrips(swap.clone())) {
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

    /// Turns the UI, as far as the screen in front allows; it comes back when
    /// another screen does.
    pub fn set_rotation(&self, rotation: Rotation) -> bool {
        self.shared.send(Event::SetRotation(rotation))
    }

    /// A theme without dark colors stays light.
    pub fn set_color_mode(&self, mode: ColorMode) -> bool {
        self.shared.send(Event::SetColorMode(mode))
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

const ROTATIONS: [Rotation; 4] = [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270];

fn rotation_code(rotation: Rotation) -> u8 {
    ROTATIONS.iter().position(|r| *r == rotation).unwrap_or(0) as u8
}

/// Keeps a picture to whole 16 row blocks, which a rotated copy needs.
fn grid_rows(rect: Rect) -> Rect {
    let top = rect.y.div_euclid(ROW_GRID) * ROW_GRID;
    let bottom = (rect.bottom() + ROW_GRID - 1).div_euclid(ROW_GRID) * ROW_GRID;
    Rect::new(rect.x, top, rect.w, bottom - top)
}

fn note(stage: &mut (u32, u32), from: Instant) -> Instant {
    let now = Instant::now();
    let micros = now.duration_since(from).as_micros() as u32;
    *stage = (micros, stage.1.max(micros));
    now
}

pub struct AppState<C: Screen> {
    shared: Arc<Shared>,
    receiver: Receiver<Event>,
    tree: Tree,
    presenter: Presenter,
    cache: GlyphCache<Caps>,
    effects: Effects,
    data_caps: Caps,
    timing: Timing,
    surface: Option<ExternalSurface>,
    hole: Option<Rect>,
    overlay: Option<Rect>,
    exclusive: bool,
    waiting: Option<Instant>,
    requested: Rotation,
    rotation: Rotation,
    rotated: bool,
    plugins: Vec<Box<dyn Plugin>>,
    context: PluginContext,
    _root: PhantomData<fn(C)>,
}

/// Runs a component tree on the display. It lives on the heap: `run` keeps it
/// for good, under every frame on the stack of the same task.
pub struct App<C: Screen>(Box<AppState<C>>);

impl<C: Screen> Deref for App<C> {
    type Target = AppState<C>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<C: Screen> DerefMut for App<C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<C: Screen> App<C> {
    // Not inlined, so what building it passes through leaves the caller's stack.
    #[inline(never)]
    pub fn new(display: &'static Display, touch: Option<&'static Touch>, root: C, config: AppConfig) -> idf::Result<Self> {
        Ok(Self(Box::new(AppState::new(display, touch, root, config)?)))
    }

    pub fn run(mut self) -> ! {
        self.0.run()
    }
}

impl<C: Screen> AppState<C> {
    fn new(display: &'static Display, touch: Option<&'static Touch>, root: C, mut config: AppConfig) -> idf::Result<Self> {
        let store = Store::new(config.nvs_namespace);
        let mut init = PluginInit { store: &store, rotation: config.presenter.rotation, color_mode: ColorMode::Light };
        for plugin in &mut config.plugins {
            plugin.init(&mut init);
        }
        let (rotation, color_mode) = (init.rotation, init.color_mode);
        config.presenter.rotation = rotation;
        let presenter = Presenter::new(display, &config.presenter)?;
        let (events, receiver) = channel(QUEUE_DEPTH);
        let shared = Arc::new(Shared {
            events,
            pending: AtomicU32::new(0),
            dropped: AtomicU32::new(0),
            busy: AtomicBool::new(true),
            display,
            rotation: AtomicU8::new(rotation_code(config.presenter.rotation)),
            touch,
        });
        if let Some(touch) = touch {
            let shared = shared.clone();
            touch.set_event_cb(Some(Box::new(move |points: &[Point]| {
                let mut frame = [Point::default(); MAX_POINTS];
                let count = points.len().min(MAX_POINTS);
                frame[..count].copy_from_slice(&points[..count]);
                let now = Instant::now().since_boot().as_millis() as u32;
                if !shared.send(Event::Touch(frame, count as u8, now)) {
                    let dropped = shared.dropped.fetch_add(1, Ordering::Relaxed);
                    if dropped % DROP_LOG_EVERY == 0 {
                        error!(TAG, "input queue full, {} samples dropped", dropped + 1);
                    }
                }
            })));
        }
        let area = presenter.area();
        let context = PluginContext { store, input: Input { shared: shared.clone() } };
        let mut app = Self {
            shared,
            receiver,
            tree: {
                let mut tree = Tree::new(root, area, config.presenter.data_caps, config.theme);
                tree.add_keyboard(config.keyboard);
                tree.color_mode = color_mode;
                tree.set_pixel_format(match display.pixel_format() {
                    PixelFormat::Rgb888 => ImageFormat::Rgb888,
                    PixelFormat::L8 => ImageFormat::L8,
                    PixelFormat::Rgb565 => ImageFormat::Rgb565,
                });
                tree
            },
            presenter,
            cache: GlyphCache::new_in(config.glyph_cache_bytes, config.glyph_cache_caps),
            effects: Effects::new(config.presenter.data_caps),
            data_caps: config.presenter.data_caps,
            timing: Timing::default(),
            surface: None,
            hole: None,
            overlay: None,
            exclusive: false,
            waiting: None,
            requested: rotation,
            rotation,
            rotated: false,
            plugins: config.plugins,
            context,
            _root: PhantomData,
        };
        let _ = app.tree.link.set(app.shared.clone());
        for plugin in &mut app.plugins {
            plugin.start(&app.context);
        }
        Ok(app)
    }

    fn notify(&mut self, event: UiEvent) {
        for plugin in &mut self.plugins {
            plugin.on_event(event, &self.context);
        }
    }

    /// Adds a component along the top above the screens that stays across
    /// navigation. How tall it comes out is the top inset of the screens.
    pub fn chrome<B: Component>(&mut self, component: B) -> Handle<B::Msg> {
        let slot = self.tree.add_chrome(component);
        Handle::new(self.tree.link.clone(), self.tree.host_id(slot))
    }

    /// `Presenter::set_strips` before `run`.
    pub fn set_strips(&mut self, strips: Strips, lanes: usize) -> Result<Strips, Strips> {
        self.presenter.set_strips(strips, lanes)
    }

    pub fn handle(&self) -> Handle<C::Msg> {
        Handle::new(self.tree.link.clone(), self.tree.root_id)
    }

    pub fn input(&self) -> Input {
        Input { shared: self.shared.clone() }
    }

    pub fn probe(&self) -> Probe {
        Probe { shared: self.shared.clone() }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Wake => {}
            Event::SetRotation(rotation) => self.requested = rotation,
            Event::SetColorMode(mode) => {
                if mode != self.tree.color_mode {
                    self.tree.set_color_mode(mode);
                    self.presenter.invalidate_all();
                    self.notify(UiEvent::ColorMode(mode));
                }
            }
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
            Event::Touch(frame, count, sampled_ms) => {
                let mut points = [(0, (0, 0)); MAX_POINTS];
                for (point, sample) in points.iter_mut().zip(&frame[..usize::from(count)]) {
                    *point = (sample.id, self.shared.to_logical(sample.x, sample.y));
                }
                self.tree.touch(&points[..usize::from(count)], sampled_ms);
            }
            Event::Post(id, msg) => self.tree.post(id, msg),
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

    fn follow_rotation(&mut self) {
        if self.tree.transiting() {
            return;
        }
        let size = self.shared.display.size();
        let panel = (size.width as i16, size.height as i16);
        let next = self.tree.screen_style.rotations.pick(self.requested, self.rotation, panel);
        if next == self.rotation {
            return;
        }
        if let Err(err) = self.presenter.set_rotation(next) {
            error!(TAG, "rotation: {err}");
            self.requested = self.rotation;
            return;
        }
        self.rotation = next;
        self.shared.rotation.store(rotation_code(next), Ordering::Release);
        self.tree.set_area(self.presenter.area());
        self.rotated = true;
        self.overlay = None;
        self.notify(UiEvent::Rotated(next));
        if let Some(surface) = &self.surface {
            surface.set_rotation(next);
            surface.set_overlay(None);
        }
        self.tree.update();
    }

    fn attach_surface(&mut self) -> Option<(ExternalSurface, Option<Rect>)> {
        let shown = self.tree.external().map(|(surface, hole)| (surface.clone(), hole));
        let attached = self.surface.as_ref();
        if attached.is_none_or(|a| shown.as_ref().is_none_or(|s| !s.0.same(a))) {
            if let Some(old) = self.surface.take() {
                old.detach();
                self.presenter.set_external(None, false);
                (self.hole, self.overlay) = (None, None);
            }
            if let Some((surface, _)) = &shown {
                let shared = self.shared.clone();
                let wake = Arc::new(move || {
                    shared.send(Event::Wake);
                });
                surface.attach(self.shared.display, self.rotation, self.presenter.buffers(), wake);
                self.surface = shown.as_ref().map(|s| s.0.clone());
            }
        }
        shown
    }

    /// `true` when the frame must wait: a surface still writing where the UI
    /// would draw next would paint over it.
    fn move_hole(&mut self, surface: &ExternalSurface, hole: Option<Rect>) -> bool {
        if hole == self.hole {
            self.waiting = None;
            return false;
        }
        surface.set_rect(hole);
        let old = self.hole.unwrap_or_default();
        let vacated = hole.is_none_or(|new| old.subtract(&new).next().is_some()) && !old.is_empty();
        if vacated && surface.writing_outdated() {
            let since = *self.waiting.get_or_insert_with(Instant::now);
            if since.elapsed() < HOLE_WAIT {
                return true;
            }
            error!(TAG, "a surface kept writing where its rectangle was");
        }
        self.waiting = None;
        if let Some(new) = hole {
            new.subtract(&old).for_each(|exposed| surface.request_refresh(exposed));
        }
        self.hole = hole;
        false
    }

    fn frame(&mut self) {
        let start = Instant::now();
        self.tree.update();
        self.follow_rotation();
        self.apply_render_mode();
        if self.tree.capture_pending() {
            self.tree.capture_presences(&mut self.effects, &mut self.cache);
        }
        if self.tree.zoom_pending() {
            match self.shared.display.blitter(2) {
                Ok(mut blitter) => {
                    let shown = self.presenter.shown().map(|fb| (fb, self.rotation));
                    let mut engine = Engine { blitter: &mut blitter, shown };
                    self.tree.capture_zooms(&mut self.effects, &mut self.cache, Some(&mut engine));
                }
                Err(err) => {
                    error!(TAG, "blitter: {err}");
                    self.tree.capture_zooms(&mut self.effects, &mut self.cache, None);
                }
            }
        }
        let mut update = (0, 0);
        let updated = note(&mut update, start);
        let damage = self.tree.take_damage();
        let shown = self.attach_surface();
        let mut waited = self.waiting.is_some();
        if let Some((surface, hole)) = &shown {
            if surface.policy() == Policy::Exclusive {
                let whole = Some(self.presenter.area());
                self.presenter.set_external(whole, true);
                surface.set_rect(whole);
                self.hole = whole;
                if !core::mem::replace(&mut self.exclusive, true) || core::mem::take(&mut self.rotated) {
                    surface.request_refresh(self.presenter.area());
                }
                for rect in damage {
                    self.presenter.invalidate(rect);
                }
                return;
            }
            if core::mem::take(&mut self.exclusive) {
                self.presenter.invalidate_all();
                waited = true;
            }
            if self.move_hole(surface, *hole) {
                for rect in damage {
                    self.presenter.invalidate(rect);
                }
                return;
            }
            if let (true, Some(hole)) = (core::mem::take(&mut self.rotated), hole) {
                surface.request_refresh(*hole);
            }
        }
        if damage.is_empty() && !self.presenter.has_new_damage() && !waited && !self.presenter.behind() {
            return;
        }
        self.timing.frames += 1;
        self.timing.update = (update.0, self.timing.update.1.max(update.0));
        for rect in &damage {
            self.presenter.invalidate(*rect);
        }
        let mut list = DisplayList::new_in(self.data_caps);
        let regions = match self.presenter.regions() {
            Ok(regions) => regions,
            Err(err) => {
                error!(TAG, "acquire: {err}");
                return;
            }
        };
        self.tree.paint(&mut self.effects, regions, &mut list);
        let mut overlap = None;
        if let Some((surface, hole)) = &shown {
            overlap = hole.and_then(|h| devkit_gfx::overlap(list.ops(), self.tree.external_split.get(), &h).map(|o| grid_rows(o).intersect(&h)));
            let composite = (surface.policy() == Policy::Stamp || overlap.is_some()) && self.presenter.buffers().count >= 2;
            surface.set_composite(composite);
            if self.presenter.set_external(*hole, composite) {
                list.clear();
                if let Ok(regions) = self.presenter.regions() {
                    self.tree.paint(&mut self.effects, regions, &mut list);
                }
            }
            if composite {
                if let Some(hole) = hole {
                    surface.request_refresh(*hole);
                }
            }
        }
        let painted = note(&mut self.timing.paint, updated);
        match self.presenter.render(&list, &mut self.cache) {
            Ok(stats) if stats.glyphs_missing > 0 => {
                error!(TAG, "{} glyphs did not fit the cache", stats.glyphs_missing);
            }
            Ok(_) => {}
            Err(err) => error!(TAG, "render: {err}"),
        }
        note(&mut self.timing.render, painted);
        if let Some((surface, _)) = &shown {
            let changed = overlap != self.overlay || overlap.is_some_and(|o| damage.iter().any(|d| d.intersects(&o)));
            if changed {
                match overlap {
                    Some(rect) => match self.presenter.render_overlay(&list, self.tree.external_split.get(), rect, &mut self.cache) {
                        Ok(overlay) => surface.set_overlay(Some(Arc::new(overlay))),
                        Err(err) => error!(TAG, "overlay: {err}"),
                    },
                    None => surface.set_overlay(None),
                }
                if let Some(area) = overlap.or(self.overlay) {
                    surface.request_refresh(area);
                }
                self.overlay = overlap;
            }
        }
    }

    /// Runs on the calling task, which then owns the UI.
    fn run(&mut self) -> ! {
        let mut last = Instant::now();
        loop {
            self.frame();
            self.shared.busy.store(!self.tree.idle(), Ordering::Release);
            // While animating the presenter paces frames; a timed wait here
            // would add at least two ticks to each.
            let wait = if self.tree.timed() || self.waiting.is_some() {
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
            // The rest of a millisecond stays in `last`: frames shorter than one
            // would otherwise never add up, and what moves by itself would stop.
            let elapsed = last.elapsed().as_millis() as u32;
            self.tree.tick(elapsed);
            last += Duration::from_millis(u64::from(elapsed));
        }
    }
}
