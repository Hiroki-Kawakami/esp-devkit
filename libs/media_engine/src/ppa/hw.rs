/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::alloc::{Allocator, Layout};
use core::ffi::{CStr, c_void};
use core::mem;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU32, Ordering, fence};
use core::task::{Context, Poll, Waker};
use core::time::Duration;

use devkit_gfx::{ImageFormat, Rotation};
use idf::cache::{self, Flags};
use idf::heap::Caps;
use idf::log::error;
use idf::sync::Spinlock;
use idf::time::Instant;
use idf::{Error, Result, thread};

use super::{Alpha, In, Kind, Out, Plan, Yuv, YuvRange, YuvStandard, scaled};
use crate::sys;

const TAG: &CStr = c"ppa";
/// What a drop waits before it stops a job the engine never finished.
const DROP_TIMEOUT: Duration = Duration::from_secs(1);
const DESCRIPTORS: usize = 3;

static MB_ROWS: AtomicU32 = AtomicU32::new(16);

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Copy)]
struct PicIn {
    buffer: *const u8,
    pic: (u32, u32),
    block: (u32, u32, u32, u32),
    cm: u32,
    bits: usize,
    yuv: Option<(u32, u32)>,
    rgb_swap: bool,
    byte_swap: bool,
    alpha: (u32, u32),
}

#[derive(Clone, Copy)]
struct PicOut {
    buffer: *mut u8,
    pic: (u32, u32),
    at: (u32, u32),
    cm: u32,
    bits: usize,
    yuv: Option<(u32, u32)>,
}

/// What the engine start writes, worked out when the job was queued.
#[derive(Clone, Copy)]
struct Setup {
    kind: Kind,
    engine: *const Engine,
    input: PicIn,
    fg: PicIn,
    fg_alpha_only: bool,
    fg_rgb: (u8, u8, u8),
    out: PicOut,
    rotation: u32,
    scale_x: (u32, u32),
    scale_y: (u32, u32),
    mirror_x: bool,
    mirror_y: bool,
    yuv444_in: bool,
    yuv444_std: u32,
    bypass_mb_order: bool,
    fill: (u32, u32, u32),
}

pub(super) struct Node {
    setup: Setup,
    trans: sys::dma2d_trans_config_t,
    engine: Option<&'static Engine>,
    state: State,
    waker: Option<Waker>,
    next: *mut Node,
}

struct Queue {
    running: *mut Node,
    head: *mut Node,
    tail: *mut Node,
    busy: bool,
}

unsafe impl Send for Queue {}

struct Engine {
    queue: Spinlock<Queue>,
    pool: sys::dma2d_pool_handle_t,
    placeholder: *mut sys::dma2d_trans_t,
    /// What the 2D-DMA is given; the CPU writes them through `desc_cpu`.
    desc: [*mut sys::dma2d_descriptor_t; DESCRIPTORS],
    desc_cpu: [*mut sys::dma2d_descriptor_t; DESCRIPTORS],
}

unsafe impl Send for Engine {}
unsafe impl Sync for Engine {}

impl Node {
    pub(super) fn new() -> Self {
        Self {
            setup: unsafe { mem::zeroed() },
            trans: unsafe { mem::zeroed() },
            engine: None,
            state: State::Done,
            waker: None,
            next: ptr::null_mut(),
        }
    }

    fn engine(&self) -> &'static Engine {
        self.engine.expect("node never submitted")
    }
}

unsafe impl Send for Node {}

pub(super) struct Engines {
    srm: Engine,
    blend: Engine,
}

fn internal(size: usize, align: usize) -> Result<*mut u8> {
    let layout = Layout::from_size_align(size, align).map_err(|_| Error::INVALID_ARG)?;
    let block = (Caps::DMA | Caps::INTERNAL).allocate_zeroed(layout).map_err(|_| Error::NO_MEM)?;
    Ok(block.as_ptr().cast())
}

impl Engine {
    fn new(pool: sys::dma2d_pool_handle_t) -> Result<Self> {
        let align = cache::alignment(Caps::DMA | Caps::INTERNAL);
        let desc_size = mem::size_of::<sys::dma2d_descriptor_t>().next_multiple_of(align);
        let mut desc: [*mut sys::dma2d_descriptor_t; DESCRIPTORS] = [ptr::null_mut(); DESCRIPTORS];
        let mut desc_cpu = [ptr::null_mut(); DESCRIPTORS];
        for (d, cpu) in desc.iter_mut().zip(&mut desc_cpu) {
            *d = internal(desc_size, align)?.cast();
            // Nothing cached of them may be written back over what the CPU
            // writes through the uncached alias from here on.
            unsafe { cache::msync(d.cast(), desc_size, Flags::C2M | Flags::INVALIDATE) }?;
            *cpu = cache::uncached(*d);
        }
        let placeholder = internal(unsafe { sys::dma2d_get_trans_elm_size() }, 4)?.cast();
        let queue = Queue { running: ptr::null_mut(), head: ptr::null_mut(), tail: ptr::null_mut(), busy: false };
        Ok(Self { queue: Spinlock::new(queue), pool, placeholder, desc, desc_cpu })
    }
}

impl Engines {
    pub(super) fn new() -> Result<Self> {
        unsafe { sys::media_engine_ppa_clock(true) };
        let mb = unsafe { sys::ppa_ll_srm_get_mb_size(&raw mut sys::PPA) };
        MB_ROWS.store(if mb == sys::ppa_ll_srm_mb_size_t_PPA_LL_SRM_MB_SIZE_16_16 { 16 } else { 32 }, Ordering::Relaxed);
        let config = sys::dma2d_pool_config_t { pool_id: 0, intr_priority: 0 };
        let mut pool = ptr::null_mut();
        if let Err(err) = Error::check(unsafe { sys::dma2d_acquire_pool(&config, &mut pool) }) {
            error!(TAG, "2D-DMA pool: {err}");
            return Err(err);
        }
        Ok(Self { srm: Engine::new(pool)?, blend: Engine::new(pool)? })
    }

    pub(super) fn submit(&'static self, plan: Plan, node: &mut Node) -> Result<Pending> {
        let engine = if plan.kind == Kind::Srm { &self.srm } else { &self.blend };
        let mut setup = setup(&plan)?;
        setup.engine = engine;
        let (tx, flags) = match plan.kind {
            Kind::Srm => (1, if setup.yuv444_in { sys::DMA2D_CHANNEL_FUNCTION_FLAG_TX_CSC } else { 0 }),
            Kind::Blend => (2, 0),
            Kind::Fill => (0, 0),
        };
        sync_caches(&plan)?;
        *node = Node { setup, trans: unsafe { mem::zeroed() }, engine: Some(engine), state: State::Queued, waker: None, next: ptr::null_mut() };
        let node: *mut Node = node;
        unsafe {
            let trans = &mut (*node).trans;
            trans.tx_channel_num = tx;
            trans.rx_channel_num = 1;
            trans.channel_flags = flags;
            trans.on_job_picked = Some(picked);
            trans.user_config = (&raw mut (*node).setup).cast();
        }
        let start = engine.queue.with(|q| {
            if q.busy {
                if q.tail.is_null() {
                    q.head = node;
                } else {
                    unsafe { (*q.tail).next = node };
                }
                q.tail = node;
                false
            } else {
                q.busy = true;
                q.running = node;
                unsafe { (*node).state = State::Running };
                true
            }
        });
        if start {
            start_running(engine, node);
        }
        Ok(Pending { node: NonNull::new(node).ok_or(Error::NO_MEM)? })
    }
}

/// Hands `node`, already marked running, to the 2D-DMA, and those after it
/// that fail to go.
fn start_running(engine: &'static Engine, mut node: *mut Node) {
    loop {
        let err = unsafe { sys::dma2d_enqueue(engine.pool, &(*node).trans, engine.placeholder) };
        if Error::check(err).is_ok() {
            return;
        }
        match finish(engine, State::Failed) {
            Some(next) => node = next,
            None => return,
        }
    }
}

/// Ends the running node as `state` and makes the next one running.
fn finish(engine: &'static Engine, state: State) -> Option<*mut Node> {
    let mut waker = None;
    let next = engine.queue.with(|q| {
        let done = q.running;
        if done.is_null() {
            return None;
        }
        unsafe {
            (*done).state = state;
            waker = (*done).waker.clone();
        }
        let next = q.head;
        if next.is_null() {
            q.busy = false;
            q.running = ptr::null_mut();
            return None;
        }
        unsafe {
            q.head = (*next).next;
            if q.head.is_null() {
                q.tail = ptr::null_mut();
            }
            (*next).state = State::Running;
        }
        q.running = next;
        Some(next)
    });
    if let Some(waker) = waker {
        waker.wake_by_ref();
    }
    next
}

unsafe extern "C" fn done(
    _chan: sys::dma2d_channel_handle_t,
    _event: *mut sys::dma2d_event_data_t,
    arg: *mut c_void,
) -> bool {
    let engine: &'static Engine = unsafe { &*arg.cast() };
    if let Some(next) = finish(engine, State::Done) {
        start_running(engine, next);
    }
    false
}

pub(super) struct Pending {
    node: NonNull<Node>,
}

unsafe impl Send for Pending {}

impl Pending {
    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let node = self.node.as_ptr();
        let engine = unsafe { (*node).engine() };
        engine.queue.with(|_| unsafe {
            match (*node).state {
                State::Done => Poll::Ready(Ok(())),
                State::Failed => Poll::Ready(Err(Error::FAIL)),
                _ => {
                    if !(*node).waker.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
                        (*node).waker = Some(cx.waker().clone());
                    }
                    Poll::Pending
                }
            }
        })
    }

    pub(super) fn forget_waker(&mut self) {
        let node = self.node.as_ptr();
        unsafe { (*node).engine() }.queue.with(|_| unsafe { (*node).waker = None });
    }

    fn state(&self) -> State {
        let node = self.node.as_ptr();
        unsafe { (*node).engine() }.queue.with(|_| unsafe { (*node).state })
    }

    /// Takes a queued node out; false once it has started.
    fn unqueue(&self) -> bool {
        let node = self.node.as_ptr();
        unsafe { (*node).engine() }.queue.with(|q| unsafe {
            if (*node).state != State::Queued {
                return false;
            }
            let mut prev: *mut Node = ptr::null_mut();
            let mut at = q.head;
            while !at.is_null() && at != node {
                prev = at;
                at = (*at).next;
            }
            if prev.is_null() {
                q.head = (*node).next;
            } else {
                (*prev).next = (*node).next;
            }
            if q.tail == node {
                q.tail = prev;
            }
            (*node).state = State::Failed;
            true
        })
    }

    fn force_end(&self) {
        let node = self.node.as_ptr();
        let engine = unsafe { (*node).engine() };
        let still = engine.queue.with(|q| {
            if q.running != node || unsafe { (*node).state } != State::Running {
                return false;
            }
            q.running = ptr::null_mut();
            true
        });
        if !still {
            return;
        }
        let mut yield_ = false;
        let err = unsafe { sys::dma2d_force_end(engine.placeholder, &mut yield_) };
        error!(TAG, "a job the engine did not finish in {DROP_TIMEOUT:?} was stopped ({err})");
        engine.queue.with(|q| q.running = node);
        if let Some(next) = finish(engine, State::Failed) {
            start_running(engine, next);
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.unqueue() {
            let start = Instant::now();
            while matches!(self.state(), State::Running) {
                if start.elapsed() >= DROP_TIMEOUT {
                    self.force_end();
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn span(ptr: *const u8, len: usize, pic_w: u32, format: ImageFormat, (x, y, w, h): (u32, u32, u32, u32)) -> (*const u8, usize) {
    let stride = format.row_bytes(pic_w as usize);
    let first = y as usize * stride + format.row_bytes(x as usize);
    let last = (y + h - 1) as usize * stride + format.row_bytes((x + w) as usize);
    debug_assert!(last <= len);
    (unsafe { ptr.add(first) }, last - first)
}

fn sync_caches(plan: &Plan) -> Result<()> {
    for input in [&plan.input, &plan.fg] {
        // A background in the target is covered by the output's write-back.
        if !input.ptr.is_null() && input.ptr != plan.out.ptr.cast_const() {
            let (at, len) = span(input.ptr, input.len, input.pic_w, input.format, input.block);
            // Pictures in flash have no cache to write back, and say so with an error.
            let _ = unsafe { cache::msync(at, len, Flags::C2M | Flags::UNALIGNED) };
        }
    }
    let out = &plan.out;
    let (at, len) = span(out.ptr, out.len, out.pic_w, out.format, (out.at.0, out.at.1, out.size.0, out.size.1));
    unsafe { cache::msync(at, len, Flags::C2M | Flags::INVALIDATE | Flags::UNALIGNED) }
}

fn srm_mode(format: ImageFormat, output: bool) -> Option<(u32, bool)> {
    let (cm, swap) = match format {
        ImageFormat::Argb8888 => (sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_ARGB8888, false),
        ImageFormat::Bgr888 => (sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB888, false),
        ImageFormat::Rgb888 if !output => (sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB888, true),
        ImageFormat::Rgb565 => (sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565, false),
        ImageFormat::Yuv420 => (sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_YUV420, false),
        ImageFormat::Yuv444 if !output => (sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_YUV444, false),
        ImageFormat::Uyvy => (sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_YUV422_UYVY, false),
        ImageFormat::Yuyv if !output => (sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_YUV422_YUYV, false),
        ImageFormat::L8 => (sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_GRAY8, false),
        _ => return None,
    };
    unsafe { sys::ppa_ll_srm_is_color_mode_supported(cm) }.then_some((cm, swap))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Background,
    Foreground,
    Output,
}

fn blend_mode(format: ImageFormat, side: Side) -> Option<(u32, bool)> {
    let (cm, swap) = match format {
        ImageFormat::Argb8888 => (sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_ARGB8888, false),
        ImageFormat::Bgr888 => (sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_RGB888, false),
        ImageFormat::Rgb888 if side != Side::Output => (sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_RGB888, true),
        ImageFormat::Rgb565 => (sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_RGB565, false),
        ImageFormat::A8 if side == Side::Foreground => (sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_A8, false),
        ImageFormat::Yuv420 if side != Side::Foreground => (sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_YUV420, false),
        ImageFormat::Uyvy if side != Side::Foreground => (sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_YUV422_UYVY, false),
        ImageFormat::Yuyv if side == Side::Background => (sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_YUV422_YUYV, false),
        ImageFormat::L8 if side != Side::Foreground => (sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_GRAY8, false),
        _ => return None,
    };
    unsafe { sys::ppa_ll_blend_is_color_mode_supported(cm) }.then_some((cm, swap))
}

fn fill_mode(format: ImageFormat) -> Option<u32> {
    let cm = match format {
        ImageFormat::Argb8888 => sys::ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_ARGB8888,
        ImageFormat::Bgr888 => sys::ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_RGB888,
        ImageFormat::Rgb565 => sys::ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_RGB565,
        ImageFormat::L8 => sys::ppa_fill_color_mode_t_PPA_FILL_COLOR_MODE_GRAY8,
        _ => return None,
    };
    unsafe { sys::ppa_ll_blend_is_color_mode_supported(cm) }.then_some(cm)
}

fn yuv(yuv: Yuv) -> (u32, u32) {
    let range = match yuv.range {
        YuvRange::Limited => sys::ppa_color_range_t_PPA_COLOR_RANGE_LIMIT,
        YuvRange::Full => sys::ppa_color_range_t_PPA_COLOR_RANGE_FULL,
    };
    let standard = match yuv.standard {
        YuvStandard::Bt601 => sys::ppa_color_conv_std_rgb_yuv_t_PPA_COLOR_CONV_STD_RGB_YUV_BT601,
        YuvStandard::Bt709 => sys::ppa_color_conv_std_rgb_yuv_t_PPA_COLOR_CONV_STD_RGB_YUV_BT709,
    };
    (range, standard)
}

fn alpha(alpha: Alpha) -> (u32, u32) {
    match alpha {
        Alpha::Keep => (sys::ppa_alpha_update_mode_t_PPA_ALPHA_NO_CHANGE, 0),
        Alpha::Fixed(v) => (sys::ppa_alpha_update_mode_t_PPA_ALPHA_FIX_VALUE, u32::from(v)),
        Alpha::Scale(v) => (sys::ppa_alpha_update_mode_t_PPA_ALPHA_SCALE, u32::from(v)),
        Alpha::Invert => (sys::ppa_alpha_update_mode_t_PPA_ALPHA_INVERT, 0),
    }
}

fn picture_in(input: &In, (cm, swap): (u32, bool)) -> PicIn {
    PicIn {
        buffer: input.ptr,
        pic: (input.pic_w, input.pic_h),
        block: input.block,
        cm,
        bits: input.format.bits(),
        yuv: is_yuv(input.format).then(|| yuv(input.yuv)),
        rgb_swap: input.rgb_swap != swap,
        byte_swap: input.byte_swap,
        alpha: alpha(input.alpha),
    }
}

fn picture_out(out: &Out, cm: u32) -> PicOut {
    PicOut {
        buffer: out.ptr,
        pic: (out.pic_w, out.pic_h),
        at: out.at,
        cm,
        bits: out.format.bits(),
        yuv: is_yuv(out.format).then(|| yuv(out.yuv)),
    }
}

fn is_yuv(format: ImageFormat) -> bool {
    matches!(format, ImageFormat::Yuv420 | ImageFormat::Yuv444 | ImageFormat::Yuyv | ImageFormat::Uyvy)
}

fn rotation(r: Rotation) -> u32 {
    match r {
        Rotation::R0 => sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_0,
        Rotation::R90 => sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_90,
        Rotation::R180 => sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_180,
        Rotation::R270 => sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_270,
    }
}

/// The macro-block order bypass for P4 DIG-734, as IDF works it out: a last
/// block smaller than the 2D-DMA FIFO stalls the transfer without an EOF.
fn bypass_mb_order(plan: &Plan) -> bool {
    let (_, _, w, h) = plan.input.block;
    let ((bw, bh), (sx, sy)) =
        if plan.rotation.swaps_axes() { ((h, w), (plan.scale_y, plan.scale_x)) } else { ((w, h), (plan.scale_x, plan.scale_y)) };
    let w_out = scaled(bw, sx);
    let divisor = if matches!(plan.out.format, ImageFormat::Argb8888 | ImageFormat::Bgr888) { 32 } else { 64 };
    let w_left = match w_out % divisor {
        0 => divisor,
        n => n,
    };
    let mb = MB_ROWS.load(Ordering::Relaxed);
    let h_in_left = match bh % mb {
        0 => mb,
        n => n,
    };
    let h_left = scaled(h_in_left, sy);
    // Corners up to 1800 bits were seen to stall too, so this covers more than the FIFO.
    const BYPASS_BELOW_BITS: u32 = 4096;
    (w_out > divisor || bh > mb) && w_left * h_left * (plan.out.format.bits() as u32) < BYPASS_BELOW_BITS
}

fn setup(plan: &Plan) -> Result<Setup> {
    let mut setup: Setup = unsafe { mem::zeroed() };
    setup.kind = plan.kind;
    match plan.kind {
        Kind::Srm => {
            let input = srm_mode(plan.input.format, false).ok_or(Error::NOT_SUPPORTED)?;
            let (out, _) = srm_mode(plan.out.format, true).ok_or(Error::NOT_SUPPORTED)?;
            setup.input = picture_in(&plan.input, input);
            setup.out = picture_out(&plan.out, out);
            setup.rotation = rotation(plan.rotation);
            setup.scale_x = plan.scale_x;
            setup.scale_y = plan.scale_y;
            if plan.out.format == ImageFormat::Yuv420 {
                setup.scale_x.1 &= !1;
                setup.scale_y.1 &= !1;
            } else if plan.out.format == ImageFormat::Uyvy {
                setup.scale_x.1 &= !1;
            }
            setup.mirror_x = plan.mirror_x;
            setup.mirror_y = plan.mirror_y;
            setup.yuv444_in = plan.input.format == ImageFormat::Yuv444;
            setup.yuv444_std = match plan.input.yuv.standard {
                YuvStandard::Bt601 => sys::dma2d_csc_tx_option_t_DMA2D_CSC_TX_YUV444_TO_RGB888_601,
                YuvStandard::Bt709 => sys::dma2d_csc_tx_option_t_DMA2D_CSC_TX_YUV444_TO_RGB888_709,
            };
            if setup.yuv444_in {
                setup.input.cm = sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB888;
                setup.input.yuv = None;
            }
            setup.bypass_mb_order = bypass_mb_order(plan);
        }
        Kind::Blend => {
            let bg = blend_mode(plan.input.format, Side::Background).ok_or(Error::NOT_SUPPORTED)?;
            let fg = blend_mode(plan.fg.format, Side::Foreground).ok_or(Error::NOT_SUPPORTED)?;
            let (out, _) = blend_mode(plan.out.format, Side::Output).ok_or(Error::NOT_SUPPORTED)?;
            setup.input = picture_in(&plan.input, bg);
            setup.fg = picture_in(&plan.fg, fg);
            setup.out = picture_out(&plan.out, out);
            setup.fg_alpha_only = plan.fg.format == ImageFormat::A8;
            setup.fg_rgb = (plan.fg_color.r, plan.fg_color.g, plan.fg_color.b);
        }
        Kind::Fill => {
            let out = fill_mode(plan.out.format).ok_or(Error::NOT_SUPPORTED)?;
            let c = plan.color;
            setup.out = picture_out(&plan.out, out);
            let color = if plan.out.format == ImageFormat::L8 {
                (u32::from(c.r) * 77 + u32::from(c.g) * 150 + u32::from(c.b) * 29) >> 8
            } else {
                u32::from(c.a) << 24 | u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
            };
            setup.fill = (plan.out.size.0, plan.out.size.1, color);
        }
    }
    Ok(setup)
}

fn pbyte(bits: usize) -> u32 {
    match bits {
        4 => sys::DMA2D_DESCRIPTOR_PBYTE_0B5_PER_PIXEL,
        8 => sys::DMA2D_DESCRIPTOR_PBYTE_1B0_PER_PIXEL,
        12 => sys::DMA2D_DESCRIPTOR_PBYTE_1B5_PER_PIXEL,
        16 => sys::DMA2D_DESCRIPTOR_PBYTE_2B0_PER_PIXEL,
        24 => sys::DMA2D_DESCRIPTOR_PBYTE_3B0_PER_PIXEL,
        _ => sys::DMA2D_DESCRIPTOR_PBYTE_4B0_PER_PIXEL,
    }
}

/// `dma2d_descriptor_t` as its three words of bitfields: bindgen's setters go
/// bit by bit and cost microseconds a descriptor.
#[repr(C, align(8))]
struct Descriptor {
    words: [u32; 3],
    buffer: *mut c_void,
    next: *mut c_void,
}

const _: () = assert!(mem::size_of::<Descriptor>() == mem::size_of::<sys::dma2d_descriptor_t>());

/// One single-block descriptor, through the uncached alias.
unsafe fn describe(desc: *mut sys::dma2d_descriptor_t, buffer: *const u8, pic: (u32, u32), block: (u32, u32), at: (u32, u32), bits: usize) {
    const FIELD: u32 = (1 << 14) - 1;
    let d = desc.cast::<Descriptor>();
    let owner = sys::DMA2D_DESCRIPTOR_BUFFER_OWNER_DMA;
    unsafe {
        (*d).words = [
            (block.1 & FIELD) | (block.0 & FIELD) << 14 | 1 << 29 | 1 << 30 | owner << 31,
            (pic.1 & FIELD) | (pic.0 & FIELD) << 14 | pbyte(bits) << 28,
            (at.1 & FIELD) | (at.0 & FIELD) << 14 | sys::DMA2D_DESCRIPTOR_BLOCK_RW_MODE_SINGLE << 28,
        ];
        (*d).buffer = buffer.cast_mut().cast();
        (*d).next = ptr::null_mut();
    }
    fence(Ordering::SeqCst);
}

unsafe fn connect(chan: sys::dma2d_channel_handle_t, periph: sys::dma2d_trigger_peripheral_t, sel: u32, desc: *mut sys::dma2d_descriptor_t) {
    let trig = sys::dma2d_trigger_t { periph, periph_sel_id: sel as _ };
    let ability = sys::dma2d_transfer_ability_t {
        access_ext_mem: true,
        data_burst_length: sys::ppa_data_burst_length_t_PPA_DATA_BURST_LENGTH_128 as _,
        desc_burst_en: true,
        mb_size: sys::dma2d_macro_block_size_t_DMA2D_MACRO_BLOCK_SIZE_NONE,
    };
    unsafe {
        sys::dma2d_connect(chan, &trig);
        sys::dma2d_set_transfer_ability(chan, &ability);
        sys::dma2d_set_desc_addr(chan, desc as isize);
    }
}

unsafe fn on_done(rx: sys::dma2d_channel_handle_t, setup: &Setup) {
    let mut cbs: sys::dma2d_rx_event_callbacks_t = unsafe { mem::zeroed() };
    cbs.on_recv_eof = Some(done);
    unsafe { sys::dma2d_register_rx_event_callbacks(rx, &mut cbs, setup.engine.cast_mut().cast()) };
}

fn channels(n: u32, chans: *const sys::dma2d_trans_channel_info_t) -> ([sys::dma2d_channel_handle_t; 2], sys::dma2d_channel_handle_t) {
    let mut tx = [ptr::null_mut(); 2];
    let (mut txs, mut rx) = (0, ptr::null_mut());
    for c in unsafe { core::slice::from_raw_parts(chans, n as usize) } {
        if c.dir == sys::dma2d_channel_direction_t_DMA2D_CHANNEL_DIRECTION_TX {
            if txs < 2 {
                tx[txs] = c.chan;
                txs += 1;
            }
        } else {
            rx = c.chan;
        }
    }
    (tx, rx)
}

/// The 2D-DMA calls it once it has the channels, from an interrupt when the
/// job was waiting behind another.
unsafe extern "C" fn picked(n: u32, chans: *const sys::dma2d_trans_channel_info_t, setup: *mut c_void) -> bool {
    let setup = unsafe { &*setup.cast::<Setup>() };
    let engine = unsafe { &*setup.engine };
    let ([tx, fg_tx], rx) = channels(n, chans);
    let dev = &raw mut sys::PPA;
    let (input, fg, out) = (&setup.input, &setup.fg, &setup.out);
    unsafe {
        match setup.kind {
            Kind::Srm => {
                sys::ppa_ll_srm_reset(dev);
                let (x, y, w, h) = input.block;
                describe(engine.desc_cpu[0], input.buffer, input.pic, (w, h), (x, y), input.bits);
                // A zero block trips DESC_ERROR, and YUV420 needs it even; the RX
                // block is written back whatever it says.
                describe(engine.desc_cpu[1], out.buffer, out.pic, (2, 2), out.at, out.bits);
                connect(tx, sys::dma2d_trigger_peripheral_t_DMA2D_TRIG_PERIPH_PPA_SRM, sys::SOC_DMA2D_TRIG_PERIPH_PPA_SRM_TX, engine.desc[0]);
                connect(rx, sys::dma2d_trigger_peripheral_t_DMA2D_TRIG_PERIPH_PPA_SRM, sys::SOC_DMA2D_TRIG_PERIPH_PPA_SRM_RX, engine.desc[1]);
                if setup.yuv444_in {
                    let mut csc: sys::dma2d_csc_config_t = mem::zeroed();
                    csc.__bindgen_anon_1.tx_csc_option = setup.yuv444_std;
                    sys::dma2d_configure_color_space_conversion(tx, &csc);
                }
                let (mut block_h, mut block_v) = (0, 0);
                sys::ppa_ll_srm_get_dma_dscr_port_mode_block_size(dev, input.cm, sys::ppa_ll_srm_get_mb_size(dev), &mut block_h, &mut block_v);
                let port = sys::dma2d_dscr_port_mode_config_t { block_h, block_v };
                sys::dma2d_configure_dscr_port_mode(tx, &port);
                on_done(rx, setup);
                sys::dma2d_start(tx);
                sys::dma2d_start(rx);

                sys::ppa_ll_srm_set_rx_color_mode(dev, input.cm);
                if let Some((range, standard)) = input.yuv {
                    sys::ppa_ll_srm_set_rx_yuv_range(dev, range);
                    sys::ppa_ll_srm_set_rx_yuv2rgb_std(dev, standard);
                }
                sys::ppa_ll_srm_enable_rx_byte_swap(dev, input.byte_swap);
                sys::ppa_ll_srm_enable_rx_rgb_swap(dev, input.rgb_swap);
                sys::ppa_ll_srm_configure_rx_alpha(dev, input.alpha.0, input.alpha.1);
                sys::ppa_ll_srm_set_tx_color_mode(dev, out.cm);
                if let Some((range, standard)) = out.yuv {
                    sys::ppa_ll_srm_set_tx_yuv_range(dev, range);
                    sys::ppa_ll_srm_set_tx_rgb2yuv_std(dev, standard);
                }
                sys::ppa_ll_srm_set_rotation_angle(dev, setup.rotation);
                sys::ppa_ll_srm_set_scaling_x(dev, setup.scale_x.0, setup.scale_x.1);
                sys::ppa_ll_srm_set_scaling_y(dev, setup.scale_y.0, setup.scale_y.1);
                sys::ppa_ll_srm_enable_mirror_x(dev, setup.mirror_x);
                sys::ppa_ll_srm_enable_mirror_y(dev, setup.mirror_y);
                sys::ppa_ll_srm_bypass_mb_order(dev, setup.bypass_mb_order);
                sys::ppa_ll_srm_start(dev);
            }
            Kind::Blend => {
                sys::ppa_ll_blend_reset(dev);
                let (bx, by, bw, bh) = input.block;
                let (fx, fy, fw, fh) = fg.block;
                describe(engine.desc_cpu[0], input.buffer, input.pic, (bw, bh), (bx, by), input.bits);
                describe(engine.desc_cpu[1], fg.buffer, fg.pic, (fw, fh), (fx, fy), fg.bits);
                describe(engine.desc_cpu[2], out.buffer, out.pic, (fw, fh), out.at, out.bits);
                let periph = sys::dma2d_trigger_peripheral_t_DMA2D_TRIG_PERIPH_PPA_BLEND;
                connect(tx, periph, sys::SOC_DMA2D_TRIG_PERIPH_PPA_BLEND_BG_TX, engine.desc[0]);
                connect(fg_tx, periph, sys::SOC_DMA2D_TRIG_PERIPH_PPA_BLEND_FG_TX, engine.desc[1]);
                connect(rx, periph, sys::SOC_DMA2D_TRIG_PERIPH_PPA_BLEND_RX, engine.desc[2]);
                on_done(rx, setup);
                sys::dma2d_start(tx);
                sys::dma2d_start(fg_tx);
                sys::dma2d_start(rx);

                sys::ppa_ll_blend_set_rx_bg_color_mode(dev, input.cm);
                if let Some((range, standard)) = input.yuv {
                    sys::ppa_ll_blend_set_rx_bg_yuv_range(dev, range);
                    sys::ppa_ll_blend_set_rx_bg_yuv2rgb_std(dev, standard);
                }
                sys::ppa_ll_blend_enable_rx_bg_byte_swap(dev, input.byte_swap);
                sys::ppa_ll_blend_enable_rx_bg_rgb_swap(dev, input.rgb_swap);
                sys::ppa_ll_blend_configure_rx_bg_alpha(dev, input.alpha.0, input.alpha.1);
                sys::ppa_ll_blend_set_rx_fg_color_mode(dev, fg.cm);
                if setup.fg_alpha_only {
                    let mut rgb: sys::color_pixel_rgb888_data_t = mem::zeroed();
                    (rgb.__bindgen_anon_1.r, rgb.__bindgen_anon_1.g, rgb.__bindgen_anon_1.b) = setup.fg_rgb;
                    sys::ppa_ll_blend_set_rx_fg_fix_rgb(dev, &mut rgb);
                }
                sys::ppa_ll_blend_enable_rx_fg_byte_swap(dev, fg.byte_swap);
                sys::ppa_ll_blend_enable_rx_fg_rgb_swap(dev, fg.rgb_swap);
                sys::ppa_ll_blend_configure_rx_fg_alpha(dev, fg.alpha.0, fg.alpha.1);
                sys::ppa_ll_blend_set_tx_color_mode(dev, out.cm);
                if let Some((range, standard)) = out.yuv {
                    sys::ppa_ll_blend_set_tx_yuv_range(dev, range);
                    sys::ppa_ll_blend_set_tx_rgb2yuv_std(dev, standard);
                }
                sys::ppa_ll_blend_set_block_size(dev, bw, bh);
                // An empty key range keys nothing.
                let mut min: sys::color_pixel_rgb888_data_t = mem::zeroed();
                let mut max: sys::color_pixel_rgb888_data_t = mem::zeroed();
                max.val = 0x00ff_ffff;
                sys::ppa_ll_blend_configure_rx_bg_ck_range(dev, &mut max, &mut min);
                sys::ppa_ll_blend_configure_rx_fg_ck_range(dev, &mut max, &mut min);
                sys::ppa_ll_blend_set_ck_default_rgb(dev, &mut min);
                sys::ppa_ll_blend_enable_ck_fg_bg_reverse(dev, false);
                sys::ppa_ll_blend_start(dev, sys::ppa_ll_blend_trans_mode_t_PPA_LL_BLEND_TRANS_MODE_BLEND);
            }
            Kind::Fill => {
                sys::ppa_ll_blend_reset(dev);
                let (w, h, mut color) = setup.fill;
                describe(engine.desc_cpu[0], out.buffer, out.pic, (w, h), out.at, out.bits);
                connect(rx, sys::dma2d_trigger_peripheral_t_DMA2D_TRIG_PERIPH_PPA_BLEND, sys::SOC_DMA2D_TRIG_PERIPH_PPA_BLEND_RX, engine.desc[0]);
                on_done(rx, setup);
                sys::dma2d_start(rx);
                sys::ppa_ll_blend_configure_filling_block_color(dev, out.cm, (&raw mut color).cast());
                sys::ppa_ll_blend_set_tx_color_mode(dev, out.cm);
                sys::ppa_ll_blend_set_block_size(dev, w, h);
                sys::ppa_ll_blend_start(dev, sys::ppa_ll_blend_trans_mode_t_PPA_LL_BLEND_TRANS_MODE_FILL);
            }
        }
    }
    false
}
