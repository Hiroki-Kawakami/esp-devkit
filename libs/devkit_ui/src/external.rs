/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;
use core::ffi::CStr;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

use bsp_rs::display::{Display, FrameBuffer, ImageFormat, Rotation, Ticket};
use devkit_gfx::{Argb8888, ClipStore, DisplayList, GlyphCache, Pixel, Prepared, Rect, Size, Surface, Target, draw_from};
use idf::heap::Caps;
use idf::log::error;
use idf::sync::Mutex;
use idf::{Error, Result};

use crate::blit::{self, Blend, Blit, Blitter, NewBlitter};
use crate::presenter::{Line, STRIPS_PER_LANE, Strips};

const TAG: &CStr = c"external";
const BLEND_TIMEOUT: Duration = Duration::from_millis(100);
const OVERLAY_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) const NO_BACK: usize = usize::MAX;

/// How an `ExternalSurface` shares the panel with the UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Policy {
    /// The surface takes its rectangle. It writes the framebuffer on the panel
    /// until the UI lies over it, and then `end` lays the UI on and presents.
    #[default]
    Clip,
    /// `end` always lays the UI on and presents.
    Stamp,
    /// The UI stops drawing, the surface takes the whole panel, and `end`
    /// presents. Each frame must cover it.
    Exclusive,
}

/// The framebuffers a surface and a presenter hand over between them.
#[derive(Clone)]
pub(crate) struct Buffers {
    pub(crate) latest: Arc<AtomicUsize>,
    pub(crate) back: Arc<AtomicUsize>,
    pub(crate) count: usize,
}

impl Buffers {
    /// The framebuffer of the next presented frame, the same for whoever asks
    /// until it is presented.
    pub(crate) fn claim_back(&self, display: &Display) -> usize {
        let back = self.back.load(Ordering::Acquire);
        if back != NO_BACK {
            return back;
        }
        let latest = self.latest.load(Ordering::Acquire);
        let scanning = display.scanning().unwrap_or(latest);
        let free = (0..self.count).find(|&i| i != scanning && i != latest).unwrap_or((latest + 1) % self.count);
        match self.back.compare_exchange(NO_BACK, free, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => free,
            Err(other) => other,
        }
    }
}

struct Link {
    display: &'static Display,
    rotation: Rotation,
    buffers: Buffers,
    blitter: Option<NewBlitter>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// UI to lay over a surface: `picture` with its top left at `rect`, which
/// may reach past the surface.
pub(crate) struct Overlay {
    pub(crate) picture: Arc<Picture>,
    pub(crate) rect: Rect,
}

/// UI drawn in ARGB, turned for the panel. Over `core` it is opaque, and
/// copied in the panel's format instead of blended.
pub(crate) struct Picture {
    pub(crate) _memory: Strips,
    pub(crate) pixels: *const u8,
    pub(crate) size: Size,
    pub(crate) rotation: Rotation,
    pub(crate) core: Option<Core>,
}

/// `rect` of a picture, from its top left.
pub(crate) struct Core {
    pub(crate) _memory: Strips,
    pub(crate) pixels: *const u8,
    pub(crate) format: ImageFormat,
    pub(crate) rect: Rect,
}

unsafe impl Send for Picture {}
unsafe impl Sync for Picture {}

/// A finger held by a surface, in the coordinates of `ExternalFrame::area`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TouchPoint {
    /// The controller's track id.
    pub id: u8,
    pub x: i16,
    pub y: i16,
}

type TouchCallback = Box<dyn FnMut(&[TouchPoint], u32) + Send>;

#[derive(Default)]
struct State {
    link: Option<Link>,
    policy: Policy,
    rect: Option<Rect>,
    rect_changes: u32,
    composite: bool,
    overlay: Option<Arc<Overlay>>,
    /// UI the presenter writes over the surface, without compositing.
    over: Option<Rect>,
    /// What of `over` the surface has written since the UI last took it.
    overwritten: Rect,
    /// Changes whenever what a framebuffer holds under the overlay may no
    /// longer be what `stamped` says.
    generation: u32,
    /// Per framebuffer, the `generation` whose overlay it holds untouched.
    stamped: Vec<u32>,
    writing: bool,
    writing_changes: u32,
    notified: bool,
    pending: Rect,
    on_refresh: Option<Box<dyn FnMut(Rect) + Send>>,
    on_touch: Option<TouchCallback>,
}

struct Inner {
    state: Mutex<State>,
}

/// A rectangle of the panel written by a task of its own, placed with
/// `Ui::external`. The UI never draws inside it.
#[derive(Clone)]
pub struct ExternalSurface(Arc<Inner>);

impl State {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
    }
}

impl ExternalSurface {
    pub fn new() -> Result<Self> {
        Ok(Self(Arc::new(Inner { state: Mutex::new(State::default()) })))
    }

    pub fn set_policy(&self, policy: Policy) {
        let mut state = self.0.state.lock();
        state.policy = policy;
        let wake = state.link.as_ref().map(|l| l.wake.clone());
        drop(state);
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Asks for `area` of the logical picture to be written again. Called on
    /// the UI task, when what the surface wrote is gone, and nothing waits for
    /// the answer. Playing is no reason to drop a request, as the next frame
    /// may be long in coming; a frame written first answers it. Requests made
    /// before the next `begin` count as one.
    pub fn set_on_refresh(&self, callback: impl FnMut(Rect) + Send + 'static) {
        self.0.state.lock().on_refresh = Some(Box::new(callback));
    }

    /// The fingers that landed on the surface rather than on UI over it, with
    /// every sample while one is down and once more with none after the last
    /// lifts, on the UI task; with the ms since boot it was sampled at. A
    /// finger stays the surface's wherever it moves until it lifts. Under
    /// `Policy::Exclusive` every finger is the surface's.
    pub fn set_on_touch(&self, callback: impl FnMut(&[TouchPoint], u32) + Send + 'static) {
        self.0.state.lock().on_touch = Some(Box::new(callback));
    }

    pub(crate) fn touched(&self, points: &[TouchPoint], sampled_ms: u32) {
        let callback = self.0.state.lock().on_touch.take();
        if let Some(mut callback) = callback {
            callback(points, sampled_ms);
            let mut state = self.0.state.lock();
            if state.on_touch.is_none() {
                state.on_touch = Some(callback);
            }
        }
    }

    /// Where to write the next frame; `None` while no node shows the surface.
    /// Does not wait.
    pub fn begin(&self) -> Option<ExternalFrame> {
        let mut state = self.0.state.lock();
        let link = state.link.as_ref()?;
        let rect = state.rect?;
        let (display, rotation) = (link.display, link.rotation);
        let (fb, index) = if display.frame_buffer_count() == 0 {
            (None, 0)
        } else {
            let index = if state.composite || state.policy == Policy::Exclusive {
                link.buffers.claim_back(display)
            } else {
                link.buffers.latest.load(Ordering::Acquire)
            };
            (Some(display.frame_buffer(index)?), index)
        };
        state.writing = true;
        state.writing_changes = state.rect_changes;
        state.notified = false;
        state.pending = Rect::default();
        let area = rect;
        let granted = (fb, area, rotation);
        Some(ExternalFrame { surface: self.clone(), display, granted, written: Cell::new(None), ended: false, fb, index, area, rotation })
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn attach(&self, display: &'static Display, rotation: Rotation, buffers: Buffers, blitter: Option<NewBlitter>, wake: Arc<dyn Fn() + Send + Sync>) {
        let mut state = self.0.state.lock();
        state.link = Some(Link { display, rotation, buffers, blitter, wake });
        state.invalidate();
    }

    pub(crate) fn detach(&self) {
        let mut state = self.0.state.lock();
        (state.link, state.rect, state.overlay, state.composite) = (None, None, None, false);
        (state.over, state.overwritten) = (None, Rect::default());
        state.invalidate();
    }

    pub(crate) fn policy(&self) -> Policy {
        self.0.state.lock().policy
    }

    pub(crate) fn writing(&self) -> bool {
        self.0.state.lock().writing
    }

    /// Whether a frame started for an earlier rectangle is still being written.
    pub(crate) fn writing_outdated(&self) -> bool {
        let state = self.0.state.lock();
        state.writing && state.writing_changes != state.rect_changes
    }

    pub(crate) fn set_rect(&self, rect: Option<Rect>) {
        let mut state = self.0.state.lock();
        if state.rect != rect {
            state.rect = rect;
            state.rect_changes = state.rect_changes.wrapping_add(1);
            state.invalidate();
        }
    }

    pub(crate) fn set_rotation(&self, rotation: Rotation) {
        let mut state = self.0.state.lock();
        state.rect_changes = state.rect_changes.wrapping_add(1);
        state.invalidate();
        if let Some(link) = &mut state.link {
            link.rotation = rotation;
        }
    }

    pub(crate) fn set_composite(&self, composite: bool) {
        self.0.state.lock().composite = composite;
    }

    pub(crate) fn set_over(&self, over: Option<Rect>) {
        let mut state = self.0.state.lock();
        state.over = over;
        state.overwritten = Rect::default();
    }

    /// What the surface wrote over the UI in `over` since the last call.
    pub(crate) fn take_overwritten(&self) -> Rect {
        core::mem::take(&mut self.0.state.lock().overwritten)
    }

    pub(crate) fn set_overlay(&self, overlay: Option<Arc<Overlay>>) {
        let mut state = self.0.state.lock();
        state.overlay = overlay;
        state.invalidate();
    }

    fn stamp(&self, index: usize, generation: u32) {
        let mut state = self.0.state.lock();
        if state.stamped.len() <= index {
            state.stamped.resize(index + 1, 0);
        }
        state.stamped[index] = generation;
    }

    pub(crate) fn request_refresh(&self, area: Rect) {
        let mut state = self.0.state.lock();
        state.pending = state.pending.union(&area);
        if core::mem::replace(&mut state.notified, true) {
            return;
        }
        let (area, callback) = (state.pending, state.on_refresh.take());
        drop(state);
        if let Some(mut callback) = callback {
            callback(area);
            let mut state = self.0.state.lock();
            if state.on_refresh.is_none() {
                state.on_refresh = Some(callback);
            }
        }
    }

    fn compose(&self, frame: &ExternalFrame) -> Result<()> {
        let mut state = self.0.state.lock();
        let exclusive = state.policy == Policy::Exclusive;
        let (composite, overlay, generation) = (state.composite, state.overlay.clone(), state.generation);
        let stamped = state.stamped.get(frame.index) == Some(&generation);
        let Some(link) = &state.link else { return Ok(()) };
        let (display, buffers, blitter, wake) = (link.display, link.buffers.clone(), link.blitter, link.wake.clone());
        let written = frame.written.get().unwrap_or(frame.granted.1);
        let covered = state.over.map_or(Rect::default(), |over| over.intersect(&written));
        let overwrite = !composite && !exclusive && !covered.is_empty();
        if overwrite {
            state.overwritten = state.overwritten.union(&covered);
        }
        drop(state);
        if overwrite {
            wake();
        }
        let Some(fb) = frame.granted.0.filter(|_| composite || exclusive) else {
            return Ok(());
        };
        if let (Some(overlay), false) = (overlay, exclusive) {
            let dirty = if stamped { written } else { overlay.rect }.intersect(&overlay.rect).intersect(&frame.granted.1);
            if !dirty.is_empty() {
                let mut blitter = blitter.ok_or(Error::NOT_SUPPORTED)?(1)?;
                unsafe { lay_over(&mut *blitter, &fb, frame.rotation, &overlay, dirty) }?;
            }
            self.stamp(frame.index, generation);
        }
        let latest = buffers.latest.load(Ordering::Acquire);
        while display.scanning().is_some_and(|i| i != latest) {
            display.wait_frame()?;
        }
        display.present(frame.index)?;
        buffers.latest.store(frame.index, Ordering::Release);
        buffers.back.store(NO_BACK, Ordering::Release);
        Ok(())
    }
}

/// A frame being written: `area` of the logical picture, in `fb`. `end` when
/// it is all there.
pub struct ExternalFrame {
    surface: ExternalSurface,
    display: &'static Display,
    granted: (Option<FrameBuffer>, Rect, Rotation),
    written: Cell<Option<Rect>>,
    ended: bool,
    /// `None` on a panel without framebuffers, which takes `write` instead.
    pub fb: Option<FrameBuffer>,
    /// Changes when the frame goes to another framebuffer, for writers that
    /// only send what changed.
    pub index: usize,
    pub area: Rect,
    pub rotation: Rotation,
}

impl ExternalFrame {
    /// Over what `begin` handed out, whatever the public fields say now;
    /// `None` on a panel without framebuffers.
    pub fn target(&self) -> Option<Target<'_>> {
        let (fb, area, rotation) = self.granted;
        unsafe { fb?.target() }?.rotated(rotation).crop(area)
    }

    /// `Display::write`, in the panel's coordinates: on a panel without
    /// framebuffers. `pixels` must stay unchanged until `wait` for the ticket
    /// returns.
    pub unsafe fn write(&self, rect: Rect, pixels: &[u8]) -> Result<Ticket> {
        if self.granted.0.is_some() {
            return Err(Error::INVALID_ARG);
        }
        unsafe { self.display.write(rect, pixels) }
    }

    /// Blocks until what `write` queued for `ticket` has been sent.
    pub fn wait(&self, ticket: Ticket) -> Result<()> {
        self.display.wait(ticket)
    }

    /// The panel's pixel format.
    pub fn format(&self) -> ImageFormat {
        self.display.pixel_format()
    }

    /// Narrows what the frame is taken to have written, from all of `area` to
    /// the rectangles named, so that `end` lays the UI on again only there.
    pub fn wrote(&self, rect: Rect) {
        self.written.set(Some(self.written.get().map_or(rect, |w| w.union(&rect))));
    }

    /// Lays the UI on and presents, where the policy and the UI call for it.
    pub fn end(mut self) {
        if let Err(err) = self.surface.compose(&self) {
            error!(TAG, "end: {err}");
        }
        self.ended = true;
    }
}

impl Drop for ExternalFrame {
    fn drop(&mut self) {
        let mut state = self.surface.0.state.lock();
        if !self.ended
            && let Some(stamped) = state.stamped.get_mut(self.index)
        {
            *stamped = 0;
        }
        state.writing = false;
        let wake = state.link.as_ref().map(|l| l.wake.clone());
        drop(state);
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// `clip` of `overlay`, both in the logical picture, onto `fb`.
unsafe fn lay_over(blitter: &mut dyn Blitter, fb: &FrameBuffer, rotation: Rotation, overlay: &Overlay, clip: Rect) -> Result<()> {
    let picture = &*overlay.picture;
    let turned = unsafe { fb.target() }.ok_or(Error::INVALID_ARG)?.rotated(rotation);
    let whole = Rect::new(overlay.rect.x, overlay.rect.y, picture.size.width as i16, picture.size.height as i16);
    let core = picture.core.as_ref().map_or(Rect::default(), |c| c.rect.offset(whole.x, whole.y));
    let fg = unsafe { picture_view(picture.pixels, ImageFormat::Argb8888, picture.size, picture.rotation) }?;
    for part in whole.subtract(&core).map(|r| r.intersect(&clip)).filter(|r| !r.is_empty()) {
        let src = fg.stored_rect(part.offset(-whole.x, -whole.y)).ok_or(Error::INVALID_ARG)?;
        let dst = turned.stored_rect(part).ok_or(Error::INVALID_ARG)?;
        let target = unsafe { fb.target() }.and_then(|t| t.crop(dst)).ok_or(Error::INVALID_ARG)?;
        let op = Blend { fg: unsafe { blit::image(picture.pixels, ImageFormat::Argb8888, fg.size()) }, fg_rect: src, at: (dst.x, dst.y) };
        blitter.blend(target, &op)?;
        blit::finish(blitter, BLEND_TIMEOUT)?;
    }
    let Some(opaque) = &picture.core else { return Ok(()) };
    let part = core.intersect(&clip);
    if part.is_empty() {
        return Ok(());
    }
    let size = Size::new(opaque.rect.w as u16, opaque.rect.h as u16);
    let view = unsafe { picture_view(opaque.pixels, opaque.format, size, picture.rotation) }?;
    let src = view.stored_rect(part.offset(-core.x, -core.y)).ok_or(Error::INVALID_ARG)?;
    let dst = turned.stored_rect(part).ok_or(Error::INVALID_ARG)?;
    let target = unsafe { fb.target() }.and_then(|t| t.crop(dst)).ok_or(Error::INVALID_ARG)?;
    let mut op = Blit::new(unsafe { blit::image(opaque.pixels, opaque.format, view.size()) });
    (op.src_rect, op.at) = (src, (dst.x, dst.y));
    blitter.blit(target, &op)?;
    blit::finish(blitter, BLEND_TIMEOUT)
}

/// Pixels of a logical `size` stored turned by `rotation`.
pub(crate) unsafe fn picture_view(pixels: *const u8, format: ImageFormat, size: Size, rotation: Rotation) -> Result<Target<'static>> {
    let stored = if rotation.swaps_axes() { Size::new(size.height, size.width) } else { size };
    Ok(unsafe { blit::target(pixels.cast_mut(), format, stored) }?.rotated(rotation))
}

/// Draws what `list` paints from `first` on inside `rect` into a picture to
/// lay over a surface, turned by `rotation` for a panel in `format`. Runs on
/// this task; it is only redone when that UI changes.
pub(crate) fn render_overlay(
    list: &DisplayList<'_, Caps>,
    first: usize,
    rect: Rect,
    cache: &mut GlyphCache<Caps>,
    rotation: Rotation,
    format: ImageFormat,
    blitter: NewBlitter,
) -> Result<Picture> {
    let bytes = rect.area() as usize * Argb8888::BYTES;
    let memory = Strips::allocate((bytes + size_of::<Line>()) * STRIPS_PER_LANE, Caps::SPIRAM)?;
    let [[drawn, turned], _] = memory.buffers(1).0;
    let mut prepared = Prepared::new_in(Caps::SPIRAM);
    prepared.prepare(list, &[rect], cache);
    let surface = unsafe { Surface::new(drawn, rect.w as usize * Argb8888::BYTES, rect) };
    draw_from::<Argb8888, _, _, _>(list, first, &prepared, &mut ClipStore::new_in(Caps::SPIRAM), &surface);
    let size = Size::new(rect.w as u16, rect.h as u16);
    let opaque = opaque_core(unsafe { core::slice::from_raw_parts(drawn, bytes) }, size).filter(|c| c.area() * 2 >= rect.area());
    let turned_size = if rotation.swaps_axes() { Size::new(size.height, size.width) } else { size };
    let mut blitter = blitter(1)?;
    let pixels = if rotation == Rotation::R0 {
        drawn
    } else {
        let mut op = Blit::new(unsafe { blit::image(drawn, ImageFormat::Argb8888, size) });
        op.rotation = rotation;
        blitter.blit(unsafe { blit::target(turned, ImageFormat::Argb8888, turned_size) }?, &op)?;
        blit::finish(&mut *blitter, OVERLAY_TIMEOUT)?;
        turned
    };
    let core = opaque.and_then(|local| match copy_core(&mut *blitter, pixels, turned_size, local, rotation, format) {
        Ok(core) => Some(core),
        Err(err) => {
            error!(TAG, "overlay core: {err}");
            None
        }
    });
    Ok(Picture { _memory: memory, pixels, size, rotation, core })
}

/// `local` of a picture turned by `rotation` at `pixels`, in `format`.
fn copy_core(blitter: &mut dyn Blitter, pixels: *mut u8, turned_size: Size, local: Rect, rotation: Rotation, format: ImageFormat) -> Result<Core> {
    let src = unsafe { blit::target(pixels, ImageFormat::Argb8888, turned_size) }?.rotated(rotation).stored_rect(local).ok_or(Error::INVALID_ARG)?;
    let line = size_of::<Line>();
    let memory = Strips::allocate(format.row_bytes(src.w as usize) * src.h as usize + line * 2, Caps::SPIRAM)?;
    let core = memory.aligned();
    let mut op = Blit::new(unsafe { blit::image(pixels, ImageFormat::Argb8888, turned_size) });
    op.src_rect = src;
    blitter.blit(unsafe { blit::target(core, format, Size::new(src.w as u16, src.h as u16)) }?, &op)?;
    blit::finish(blitter, OVERLAY_TIMEOUT)?;
    Ok(Core { _memory: memory, pixels: core, format, rect: local })
}

/// The largest rectangle of opaque pixels in ARGB `pixels` of `size`.
pub(crate) fn opaque_core(pixels: &[u8], size: Size) -> Option<Rect> {
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    let runs: Vec<(usize, usize)> = (0..height)
        .map(|y| {
            let row = &pixels[y * width * Argb8888::BYTES..][..width * Argb8888::BYTES];
            let (mut best, mut start) = ((0, 0), None);
            for (x, pixel) in row.chunks_exact(Argb8888::BYTES).enumerate() {
                match (pixel[3] == 0xff, start) {
                    (true, None) => start = Some(x),
                    (false, Some(from)) => {
                        if x - from > best.1 - best.0 {
                            best = (from, x);
                        }
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(from) = start
                && width - from > best.1 - best.0
            {
                best = (from, width);
            }
            best
        })
        .collect();
    let mut best: Option<(usize, Rect)> = None;
    for top in 0..height {
        let (mut left, mut right) = runs[top];
        if (right - left) * (height - top) <= best.map_or(0, |b| b.0) {
            continue;
        }
        for (bottom, &(l, r)) in runs.iter().enumerate().skip(top) {
            (left, right) = (left.max(l), right.min(r));
            if right <= left {
                break;
            }
            let area = (right - left) * (bottom + 1 - top);
            if area > best.map_or(0, |b| b.0) {
                best = Some((area, Rect::new(left as i16, top as i16, (right - left) as i16, (bottom + 1 - top) as i16)));
            }
        }
    }
    best.map(|b| b.1)
}
