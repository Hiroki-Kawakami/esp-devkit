/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use devkit::executor::Park;
use idf::executor::TaskPark;

use devkit_gfx::{Color, Image, ImageFormat, Rect, Rotation, Target};
use idf::{Error, Result};

#[cfg(target_os = "none")]
#[path = "hw.rs"]
mod backend;
#[cfg(not(target_os = "none"))]
#[path = "sim.rs"]
mod backend;

/// How the engine changes the alpha of the pixels it reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Alpha {
    #[default]
    Keep,
    Fixed(u8),
    /// Multiplied by `n / 256`.
    Scale(u8),
    Invert,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum YuvRange {
    #[default]
    Limited,
    Full,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum YuvStandard {
    #[default]
    Bt601,
    Bt709,
}

/// How YUV pixels convert to and from RGB; ignored for other formats.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Yuv {
    pub range: YuvRange,
    pub standard: YuvStandard,
}

/// Scale, then rotate, then mirror `src_rect` of `src` into the target, its
/// top left at `at` of the target's logical picture. Scales go in steps of
/// 1/16 from 1/16 up.
#[derive(Clone, Copy)]
pub struct Srm<'a> {
    pub src: Image<'a>,
    pub src_rect: Rect,
    pub src_yuv: Yuv,
    pub at: (i16, i16),
    pub rotation: Rotation,
    pub scale_x: f32,
    pub scale_y: f32,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub rgb_swap: bool,
    pub byte_swap: bool,
    pub alpha: Alpha,
    pub dst_yuv: Yuv,
}

impl<'a> Srm<'a> {
    /// All of `src` at the target's top left, as it is.
    pub fn new(src: Image<'a>) -> Self {
        Self {
            src,
            src_rect: Rect::new(0, 0, src.width() as i16, src.height() as i16),
            src_yuv: Yuv::default(),
            at: (0, 0),
            rotation: Rotation::R0,
            scale_x: 1.0,
            scale_y: 1.0,
            mirror_x: false,
            mirror_y: false,
            rgb_swap: false,
            byte_swap: false,
            alpha: Alpha::Keep,
            dst_yuv: Yuv::default(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Background<'a> {
    /// What the target holds there.
    Target,
    Image { image: Image<'a>, at: (i16, i16), yuv: Yuv },
}

/// `size` of `fg` from `fg_at` laid over the background into the target at
/// `at`. The target must not be rotated; an `A8` foreground takes `fg_color`.
#[derive(Clone, Copy)]
pub struct Blend<'a> {
    pub bg: Background<'a>,
    pub bg_alpha: Alpha,
    pub fg: Image<'a>,
    pub fg_at: (i16, i16),
    pub fg_alpha: Alpha,
    pub fg_color: Color,
    pub size: (i16, i16),
    pub at: (i16, i16),
    pub dst_yuv: Yuv,
}

impl<'a> Blend<'a> {
    /// All of `fg` over what the target holds at `at`.
    pub fn new(fg: Image<'a>, at: (i16, i16)) -> Self {
        Self {
            bg: Background::Target,
            bg_alpha: Alpha::Keep,
            fg,
            fg_at: (0, 0),
            fg_alpha: Alpha::Keep,
            fg_color: Color::rgb(0, 0, 0),
            size: (fg.width() as i16, fg.height() as i16),
            at,
            dst_yuv: Yuv::default(),
        }
    }
}

/// `rect` of the target's logical picture in `color`.
#[derive(Clone, Copy)]
pub struct Fill {
    pub rect: Rect,
    pub color: Color,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Srm,
    Blend,
    Fill,
}

#[derive(Clone, Copy)]
pub(crate) struct In {
    pub ptr: *const u8,
    pub len: usize,
    pub pic_w: u32,
    #[cfg_attr(not(target_os = "none"), allow(dead_code))]
    pub pic_h: u32,
    pub block: (u32, u32, u32, u32),
    pub format: ImageFormat,
    pub yuv: Yuv,
    pub rgb_swap: bool,
    pub byte_swap: bool,
    pub alpha: Alpha,
}

#[derive(Clone, Copy)]
pub(crate) struct Out {
    pub ptr: *mut u8,
    pub len: usize,
    pub pic_w: u32,
    pub pic_h: u32,
    pub at: (u32, u32),
    pub size: (u32, u32),
    pub format: ImageFormat,
    pub yuv: Yuv,
}

/// One operation, checked and in the engine's terms: stored coordinates and
/// the turn and mirror that land the picture as the target's logical one.
#[derive(Clone, Copy)]
pub(crate) struct Plan {
    pub kind: Kind,
    pub input: In,
    pub fg: In,
    pub fg_color: Color,
    pub out: Out,
    pub rotation: Rotation,
    pub scale_x: (u32, u32),
    pub scale_y: (u32, u32),
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub color: Color,
}

pub(crate) const SCALE_FRAG: u32 = 16;
const SCALE_INT_MAX: u32 = 256;

fn quarter_turns(r: Rotation) -> u8 {
    match r {
        Rotation::R0 => 0,
        Rotation::R90 => 1,
        Rotation::R180 => 2,
        Rotation::R270 => 3,
    }
}

fn from_quarter_turns(n: u8) -> Rotation {
    match n % 4 {
        0 => Rotation::R0,
        1 => Rotation::R90,
        2 => Rotation::R180,
        _ => Rotation::R270,
    }
}

fn scale(s: f32) -> Result<(u32, u32)> {
    if !(s >= 1.0 / SCALE_FRAG as f32 && s < SCALE_INT_MAX as f32) {
        return Err(Error::INVALID_ARG);
    }
    Ok((s as u32, (s * SCALE_FRAG as f32) as u32 & (SCALE_FRAG - 1)))
}

pub(crate) fn scaled(n: u32, (int, frag): (u32, u32)) -> u32 {
    int * n + frag * n / SCALE_FRAG
}

fn even(v: u32) -> bool {
    v % 2 == 0
}

fn input(image: &Image<'_>, rect: Rect, yuv: Yuv, rgb_swap: bool, byte_swap: bool, alpha: Alpha) -> Result<In> {
    let bounds = Rect::new(0, 0, image.width() as i16, image.height() as i16);
    if rect.is_empty() || rect.x < 0 || rect.y < 0 || !bounds.contains(&rect) {
        return Err(Error::INVALID_ARG);
    }
    let format = image.format();
    let (pic_w, pic_h) = (image.stride() as u32, u32::from(image.height()));
    let block = (rect.x as u32, rect.y as u32, rect.w as u32, rect.h as u32);
    let (x, y, w, h) = block;
    match format {
        ImageFormat::Yuv420 if !(even(pic_w) && even(pic_h) && even(x) && even(y) && even(w) && even(h)) => {
            return Err(Error::INVALID_ARG);
        }
        ImageFormat::Yuyv | ImageFormat::Uyvy if !(even(pic_w) && even(x) && even(w)) => return Err(Error::INVALID_ARG),
        _ => {}
    }
    if byte_swap && !matches!(format, ImageFormat::Argb8888 | ImageFormat::Rgb565) {
        return Err(Error::INVALID_ARG);
    }
    if rgb_swap && !matches!(format, ImageFormat::Argb8888 | ImageFormat::Bgr888 | ImageFormat::Rgb888 | ImageFormat::Rgb565) {
        return Err(Error::INVALID_ARG);
    }
    let len = image.pixels().len();
    Ok(In { ptr: image.pixels().as_ptr(), len, pic_w, pic_h, block, format, yuv, rgb_swap, byte_swap, alpha })
}

fn output(target: &mut Target<'_>, stored: Rect, yuv: Yuv) -> Result<Out> {
    let format = target.format();
    let (pic_w, pic_h) = (target.stride() as u32, u32::from(target.size().height));
    let at = (stored.x as u32, stored.y as u32);
    match format {
        ImageFormat::Yuv420 if !(even(pic_w) && even(pic_h) && even(at.0) && even(at.1)) => return Err(Error::INVALID_ARG),
        ImageFormat::Yuyv | ImageFormat::Uyvy if !(even(pic_w) && even(at.0)) => return Err(Error::INVALID_ARG),
        _ => {}
    }
    Ok(Out {
        ptr: target.as_ptr(),
        len: target.len(),
        pic_w,
        pic_h,
        at,
        size: (stored.w as u32, stored.h as u32),
        format,
        yuv,
    })
}

fn none_in() -> In {
    In {
        ptr: core::ptr::null(),
        len: 0,
        pic_w: 0,
        pic_h: 0,
        block: (0, 0, 0, 0),
        format: ImageFormat::Argb8888,
        yuv: Yuv::default(),
        rgb_swap: false,
        byte_swap: false,
        alpha: Alpha::Keep,
    }
}

impl Srm<'_> {
    fn plan(&self, target: &mut Target<'_>) -> Result<Plan> {
        let input = input(&self.src, self.src_rect, self.src_yuv, self.rgb_swap, self.byte_swap, self.alpha)?;
        let (sx, sy) = (scale(self.scale_x)?, scale(self.scale_y)?);
        let (w, h) = (scaled(input.block.2, sx), scaled(input.block.3, sy));
        if w == 0 || h == 0 {
            return Err(Error::INVALID_ARG);
        }
        let (lw, lh) = if self.rotation.swaps_axes() { (h, w) } else { (w, h) };
        let logical = Rect::new(self.at.0, self.at.1, lw as i16, lh as i16);
        let stored = target.stored_rect(logical).ok_or(Error::INVALID_ARG)?;
        let turned = target.rotation().swaps_axes();
        let (mirror_x, mirror_y) = if turned { (self.mirror_y, self.mirror_x) } else { (self.mirror_x, self.mirror_y) };
        let rotation = from_quarter_turns(quarter_turns(self.rotation) + quarter_turns(target.rotation()));
        Ok(Plan {
            kind: Kind::Srm,
            input,
            fg: none_in(),
            fg_color: Color::rgb(0, 0, 0),
            out: output(target, stored, self.dst_yuv)?,
            rotation,
            scale_x: sx,
            scale_y: sy,
            mirror_x,
            mirror_y,
            color: Color::rgb(0, 0, 0),
        })
    }
}

impl Blend<'_> {
    fn plan(&self, target: &mut Target<'_>) -> Result<Plan> {
        if target.rotation() != Rotation::R0 {
            return Err(Error::NOT_SUPPORTED);
        }
        let logical = Rect::new(self.at.0, self.at.1, self.size.0, self.size.1);
        let stored = target.stored_rect(logical).ok_or(Error::INVALID_ARG)?;
        let fg_rect = Rect::new(self.fg_at.0, self.fg_at.1, self.size.0, self.size.1);
        let fg = input(&self.fg, fg_rect, Yuv::default(), false, false, self.fg_alpha)?;
        let out = output(target, stored, self.dst_yuv)?;
        let bg = match self.bg {
            Background::Target => In {
                ptr: out.ptr,
                len: out.len,
                pic_w: out.pic_w,
                pic_h: out.pic_h,
                block: (out.at.0, out.at.1, out.size.0, out.size.1),
                format: out.format,
                yuv: out.yuv,
                rgb_swap: false,
                byte_swap: false,
                alpha: self.bg_alpha,
            },
            Background::Image { image, at, yuv } => {
                input(&image, Rect::new(at.0, at.1, self.size.0, self.size.1), yuv, false, false, self.bg_alpha)?
            }
        };
        Ok(Plan {
            kind: Kind::Blend,
            input: bg,
            fg,
            fg_color: self.fg_color,
            out,
            rotation: Rotation::R0,
            scale_x: (1, 0),
            scale_y: (1, 0),
            mirror_x: false,
            mirror_y: false,
            color: Color::rgb(0, 0, 0),
        })
    }
}

impl Fill {
    fn plan(&self, target: &mut Target<'_>) -> Result<Plan> {
        let stored = target.stored_rect(self.rect).ok_or(Error::INVALID_ARG)?;
        Ok(Plan {
            kind: Kind::Fill,
            input: none_in(),
            fg: none_in(),
            fg_color: Color::rgb(0, 0, 0),
            out: output(target, stored, Yuv::default())?,
            rotation: Rotation::R0,
            scale_x: (1, 0),
            scale_y: (1, 0),
            mirror_x: false,
            mirror_y: false,
            color: self.color,
        })
    }
}

/// The chip's 2D engine: scale-rotate-mirror on one side, blend and fill on
/// the other, each serving its operations in the order they came.
pub struct Ppa {
    engines: backend::Engines,
}

static PPA: idf::sync::OnceLock<Ppa> = idf::sync::OnceLock::new();

/// The engine, set up on first use.
pub fn ppa() -> Result<&'static Ppa> {
    PPA.get_or_try_init(|| Ok(Ppa { engines: backend::Engines::new()? }))
}

/// Where a job waits in the engine's queue, lent to it until the job is gone,
/// so queuing needs no allocation. One slot serves one job at a time.
pub struct Slot {
    node: backend::Node,
}

impl Slot {
    pub fn new() -> Self {
        Self { node: backend::Node::new() }
    }
}

impl Default for Slot {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppa {
    /// Writes into `target` until the job is done; see [`Job`].
    pub fn srm<'a>(&'static self, op: &Srm<'a>, target: &'a mut Target<'_>, slot: &'a mut Slot) -> Result<Job<'a>> {
        let plan = op.plan(target)?;
        self.submit(plan, slot)
    }

    pub fn blend<'a>(&'static self, op: &Blend<'a>, target: &'a mut Target<'_>, slot: &'a mut Slot) -> Result<Job<'a>> {
        let plan = op.plan(target)?;
        self.submit(plan, slot)
    }

    pub fn fill<'a>(&'static self, op: &Fill, target: &'a mut Target<'_>, slot: &'a mut Slot) -> Result<Job<'a>> {
        let plan = op.plan(target)?;
        self.submit(plan, slot)
    }

    fn submit<'a>(&'static self, plan: Plan, slot: &'a mut Slot) -> Result<Job<'a>> {
        Ok(Job { inner: self.engines.submit(plan, &mut slot.node)?, _borrow: PhantomData })
    }
}

/// An operation queued or running. Await it, `wait` for it, or drop it, which
/// blocks until the engine is done with the pixels: the engine keeps writing
/// them until then, so a job must never be leaked (`mem::forget`, a leaked
/// box, an `Rc` cycle) while it runs.
pub struct Job<'a> {
    inner: backend::Pending,
    _borrow: PhantomData<&'a mut ()>,
}

impl Job<'_> {
    /// From a task that is not async.
    pub fn wait(mut self) -> Result<()> {
        // block_on would set up a whole executor for each job.
        let park = TaskPark::current();
        let waker = unsafe { Waker::from_raw(park_waker(&park)) };
        let mut cx = Context::from_waker(&waker);
        loop {
            if let Poll::Ready(result) = self.inner.poll(&mut cx) {
                self.inner.forget_waker();
                return result;
            }
            park.park(None);
        }
    }
}

/// Wakes the task that owns `park`; valid while `park` is, which `wait` keeps
/// by taking the waker back before it returns.
fn park_waker(park: &TaskPark) -> RawWaker {
    unsafe fn clone(data: *const ()) -> RawWaker {
        RawWaker::new(data, &VTABLE)
    }
    unsafe fn wake(data: *const ()) {
        unsafe { (*data.cast::<TaskPark>()).unpark() };
    }
    unsafe fn drop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake, drop);
    RawWaker::new(core::ptr::from_ref(park).cast(), &VTABLE)
}

impl Future for Job<'_> {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.get_mut().inner.poll(cx)
    }
}

#[cfg(test)]
mod tests;
