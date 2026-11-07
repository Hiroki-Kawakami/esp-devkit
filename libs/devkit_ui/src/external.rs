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

use bsp_rs::display::{Display, FrameBuffer, ImageFormat, Rotation};
use devkit_gfx::{Rect, Size, Target};
use idf::log::error;
use idf::sync::Mutex;
use idf::{Error, Result};

use media_engine::{Blend, Slot, Srm};

use crate::blit;
use crate::presenter::Strips;

const TAG: &CStr = c"external";
const BLEND_TIMEOUT: Duration = Duration::from_millis(100);

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
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// UI to lay over a surface, turned for the panel. Over `core` it is opaque,
/// and copied in the panel's format instead of blended.
pub(crate) struct Overlay {
    pub(crate) _memory: Strips,
    pub(crate) pixels: *const u8,
    pub(crate) rect: Rect,
    pub(crate) core: Option<Core>,
}

pub(crate) struct Core {
    pub(crate) _memory: Strips,
    pub(crate) pixels: *const u8,
    pub(crate) format: ImageFormat,
    pub(crate) rect: Rect,
}

unsafe impl Send for Overlay {}
unsafe impl Sync for Overlay {}

#[derive(Default)]
struct State {
    link: Option<Link>,
    policy: Policy,
    rect: Option<Rect>,
    rect_changes: u32,
    composite: bool,
    overlay: Option<Arc<Overlay>>,
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

    /// Where to write the next frame; `None` while no node shows the surface.
    /// Does not wait.
    pub fn begin(&self) -> Option<ExternalFrame> {
        let mut state = self.0.state.lock();
        let link = state.link.as_ref()?;
        let rect = state.rect?;
        let (display, rotation) = (link.display, link.rotation);
        let index = if state.composite || state.policy == Policy::Exclusive {
            link.buffers.claim_back(display)
        } else {
            link.buffers.latest.load(Ordering::Acquire)
        };
        let fb = display.frame_buffer(index)?;
        state.writing = true;
        state.writing_changes = state.rect_changes;
        state.notified = false;
        state.pending = Rect::default();
        let area = rect;
        Some(ExternalFrame { surface: self.clone(), granted: (fb, area, rotation), written: Cell::new(None), ended: false, fb, index, area, rotation })
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn attach(&self, display: &'static Display, rotation: Rotation, buffers: Buffers, wake: Arc<dyn Fn() + Send + Sync>) {
        let mut state = self.0.state.lock();
        state.link = Some(Link { display, rotation, buffers, wake });
        state.invalidate();
    }

    pub(crate) fn detach(&self) {
        let mut state = self.0.state.lock();
        (state.link, state.rect, state.overlay, state.composite) = (None, None, None, false);
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
        let state = self.0.state.lock();
        let exclusive = state.policy == Policy::Exclusive;
        let (composite, overlay, generation) = (state.composite, state.overlay.clone(), state.generation);
        let stamped = state.stamped.get(frame.index) == Some(&generation);
        let Some(link) = &state.link else { return Ok(()) };
        let (display, buffers) = (link.display, link.buffers.clone());
        drop(state);
        if !composite && !exclusive {
            return Ok(());
        }
        if let (Some(overlay), false) = (overlay, exclusive) {
            let written = frame.written.get().unwrap_or(frame.granted.1);
            let dirty = if stamped { written.intersect(&overlay.rect) } else { overlay.rect };
            if !dirty.is_empty() {
                unsafe { lay_over(&frame.fb, frame.rotation, &overlay, dirty) }?;
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
    granted: (FrameBuffer, Rect, Rotation),
    written: Cell<Option<Rect>>,
    ended: bool,
    pub fb: FrameBuffer,
    /// Changes when the frame goes to another framebuffer, for writers that
    /// only send what changed.
    pub index: usize,
    pub area: Rect,
    pub rotation: Rotation,
}

impl ExternalFrame {
    /// Over what `begin` handed out, whatever the public fields say now.
    pub fn target(&self) -> Option<Target<'_>> {
        let (fb, area, rotation) = self.granted;
        unsafe { fb.target() }?.rotated(rotation).crop(area)
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
unsafe fn lay_over(fb: &FrameBuffer, rotation: Rotation, overlay: &Overlay, clip: Rect) -> Result<()> {
    let turned = unsafe { fb.target() }.ok_or(Error::INVALID_ARG)?.rotated(rotation);
    let stored = |rect: Rect| turned.stored_rect(rect).ok_or(Error::INVALID_ARG);
    let whole = stored(overlay.rect)?;
    let ppa = media_engine::ppa()?;
    let mut slot = Slot::new();
    let core = overlay.core.as_ref().map_or(Rect::default(), |c| c.rect);
    for part in overlay.rect.subtract(&core).map(|r| r.intersect(&clip)).filter(|r| !r.is_empty()) {
        let part = stored(part)?;
        let mut target = unsafe { fb.target() }.and_then(|t| t.crop(part)).ok_or(Error::INVALID_ARG)?;
        let fg = unsafe { blit::image(overlay.pixels, ImageFormat::Argb8888, Size::new(whole.w as u16, whole.h as u16)) };
        let mut op = Blend::new(fg, (part.x, part.y));
        (op.fg_at, op.size) = ((part.x - whole.x, part.y - whole.y), (part.w, part.h));
        ppa.blend(&op, &mut target, &mut slot)?.wait_timeout(BLEND_TIMEOUT).ok_or(Error::TIMEOUT)??;
    }
    let Some(core) = &overlay.core else { return Ok(()) };
    let part = core.rect.intersect(&clip);
    if part.is_empty() {
        return Ok(());
    }
    let (from, part) = (stored(core.rect)?, stored(part)?);
    let mut target = unsafe { fb.target() }.and_then(|t| t.crop(part)).ok_or(Error::INVALID_ARG)?;
    let mut op = Srm::new(unsafe { blit::image(core.pixels, core.format, Size::new(from.w as u16, from.h as u16)) });
    (op.src_rect, op.at) = (Rect::new(part.x - from.x, part.y - from.y, part.w, part.h), (part.x, part.y));
    ppa.srm(&op, &mut target, &mut slot)?.wait_timeout(BLEND_TIMEOUT).ok_or(Error::TIMEOUT)?
}
