/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::CStr;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;

use bsp_rs::display::{self, Blender, Display, FrameBuffer, Rotation};
use devkit_gfx::Rect;
use idf::log::error;
use idf::sync::Mutex;
use idf::{Error, Result};

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

/// UI to lay over a surface, turned for the panel.
pub(crate) struct Overlay {
    pub(crate) _memory: Strips,
    pub(crate) pixels: *const u8,
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
    writing: bool,
    writing_changes: u32,
    notified: bool,
    pending: Rect,
    on_refresh: Option<Box<dyn FnMut(Rect) + Send>>,
}

struct Inner {
    state: Mutex<State>,
    blender: Mutex<Option<Blender>>,
}

/// A rectangle of the panel written by a task of its own, placed with
/// `Ui::external`. The UI never draws inside it.
#[derive(Clone)]
pub struct ExternalSurface(Arc<Inner>);

fn area_of(rect: Rect) -> display::Rect {
    display::Rect { x: rect.x as u16, y: rect.y as u16, width: rect.w as u16, height: rect.h as u16 }
}

impl ExternalSurface {
    pub fn new() -> Result<Self> {
        Ok(Self(Arc::new(Inner { state: Mutex::try_new(State::default())?, blender: Mutex::try_new(None)? })))
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
        Some(ExternalFrame { surface: self.clone(), fb, index, area: area_of(rect), rotation })
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn attach(&self, display: &'static Display, rotation: Rotation, buffers: Buffers, wake: Arc<dyn Fn() + Send + Sync>) {
        self.0.state.lock().link = Some(Link { display, rotation, buffers, wake });
    }

    pub(crate) fn detach(&self) {
        let mut state = self.0.state.lock();
        (state.link, state.rect, state.overlay, state.composite) = (None, None, None, false);
    }

    pub(crate) fn policy(&self) -> Policy {
        self.0.state.lock().policy
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
        }
    }

    pub(crate) fn set_composite(&self, composite: bool) {
        self.0.state.lock().composite = composite;
    }

    pub(crate) fn set_overlay(&self, overlay: Option<Arc<Overlay>>) {
        self.0.state.lock().overlay = overlay;
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
        let (composite, overlay) = (state.composite, state.overlay.clone());
        let Some(link) = &state.link else { return Ok(()) };
        let (display, buffers) = (link.display, link.buffers.clone());
        drop(state);
        if !composite && !exclusive {
            return Ok(());
        }
        if let (Some(overlay), false) = (overlay, exclusive) {
            let mut blender = self.0.blender.lock();
            if blender.is_none() {
                *blender = Some(Blender::new()?);
            }
            let blender = blender.as_mut().ok_or(Error::INVALID_STATE)?;
            let area = area_of(overlay.rect);
            if !unsafe { blender.blend(&frame.fb, frame.rotation, area, overlay.pixels, BLEND_TIMEOUT) }? {
                return Err(Error::TIMEOUT);
            }
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
    pub fb: FrameBuffer,
    /// Changes when the frame goes to another framebuffer, for writers that
    /// only send what changed.
    pub index: usize,
    pub area: display::Rect,
    pub rotation: Rotation,
}

impl ExternalFrame {
    /// Lays the UI on and presents, where the policy and the UI call for it.
    pub fn end(self) {
        if let Err(err) = self.surface.compose(&self) {
            error!(TAG, "end: {err}");
        }
    }
}

impl Drop for ExternalFrame {
    fn drop(&mut self) {
        let mut state = self.surface.0.state.lock();
        state.writing = false;
        let wake = state.link.as_ref().map(|l| l.wake.clone());
        drop(state);
        if let Some(wake) = wake {
            wake();
        }
    }
}
