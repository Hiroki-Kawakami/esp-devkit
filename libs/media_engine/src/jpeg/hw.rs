/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::ffi::{CStr, c_void};
use core::future::Future;
use core::mem;
use core::pin::Pin;
use core::ptr;
use core::task::{Context, Poll, Waker};
use core::time::Duration;

use devkit_gfx::ImageFormat;
use idf::cache::{self, Flags};
use idf::executor::Timer;
use idf::log::error;
use idf::sync::{Mutex, MutexGuard, MutexLock, OnceLock, Spinlock};
use idf::time::Instant;
use idf::{Error, Result};

use super::header::{self, Header, Sampling, Tables};
use super::{Chain, Frame, Layout, Output};
use crate::dma::{self, Descriptor, Fields};
use crate::ppa::{Hook, Node, YuvRange, YuvStandard};
use crate::{sys, wait};

const TAG: &CStr = c"jpeg";
/// Strips linked ahead; the pool holds one more, or a single buffer would link
/// a descriptor to itself and trip the 2D-DMA driver's idle assert.
const RX_POOL: usize = 3;

const OUT_EOF: u32 = 1 << 9;
const DECODER_EVENTS: u32 = (1 << 2)
    | (1 << 3)
    | (1 << 4)
    | (1 << 5)
    | (1 << 6)
    | (1 << 7)
    | (1 << 8)
    | OUT_EOF
    | (1 << 13)
    | (1 << 14)
    | (1 << 15)
    | (1 << 18)
    | (1 << 19)
    | (1 << 20)
    | (1 << 21)
    | (1 << 22)
    | (1 << 23)
    | (1 << 24);

/// `dec_hb_tbl` of IDF's decoder: the 2D-DMA block width for a sampling and
/// an output, 0 where the codec cannot.
const HB: [[u32; 6]; 4] = [[40, 40, 40, 32, 0, 32], [64, 32, 32, 64, 0, 32], [48, 32, 32, 48, 0, 48], [96, 0, 0, 0, 96, 0]];

/// JFIF full-range YCbCr to RGB for the 2D-DMA colour converter, which
/// computes `256 * Q = A*Y + B*Cb + C*Cr + D`; IDF only loads studio-range ones.
const FULL_BT601: [[i32; 4]; 3] = [[256, 0, 359, -45952], [256, -88, -183, 34688], [256, 454, 0, -58112]];
const FULL_BT709: [[i32; 4]; 3] = [[256, 0, 403, -51584], [256, -48, -120, 21504], [256, 475, 0, -60800]];

struct State {
    active: bool,
    strips: bool,
    strip_count: u16,
    delivered: u16,
    eof: bool,
    status: u32,
    events: u32,
    rx: sys::dma2d_channel_handle_t,
    tail: u16,
    waker: Option<Waker>,
    chain: bool,
    submitted: u16,
    completed: u16,
    failed: bool,
    submitting: u8,
}

unsafe impl Send for State {}

impl State {
    const fn new() -> Self {
        Self {
            active: false,
            strips: false,
            strip_count: 0,
            delivered: 0,
            eof: false,
            status: 0,
            events: 0,
            rx: ptr::null_mut(),
            tail: 0,
            waker: None,
            chain: false,
            submitted: 0,
            completed: 0,
            failed: false,
            submitting: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct StripGeom {
    layout: Layout,
    hb: u32,
    mcu_h: u32,
    padded_width: u32,
    padded_height: u16,
    bits: usize,
}

impl StripGeom {
    fn rows_of(&self, index: u16) -> u16 {
        self.layout.rows.min(self.padded_height - index * self.layout.rows)
    }

    fn fields(&self, index: u16) -> Fields {
        Fields {
            block: (self.hb, self.mcu_h),
            picture: (self.padded_width, u32::from(self.rows_of(index))),
            at: (0, 0),
            two_d: true,
            multiple: true,
            eof: false,
            pbyte: dma::pbyte(self.bits),
            buffer: self.layout.buffers[usize::from(index) % self.layout.count],
        }
    }
}

#[derive(Clone, Copy)]
struct Setup {
    sampling: Sampling,
    csc: u32,
    scramble: u32,
    full: Option<&'static [[i32; 4]; 3]>,
}

struct Codec {
    lock: Mutex<()>,
    hal: UnsafeCell<sys::jpeg_hal_context_t>,
    pool: sys::dma2d_pool_handle_t,
    placeholder: *mut sys::dma2d_trans_t,
    trans: UnsafeCell<sys::dma2d_trans_config_t>,
    tx: Descriptor,
    rx: [Descriptor; RX_POOL],
    setup: UnsafeCell<Setup>,
    tables: UnsafeCell<Tables>,
    geom: UnsafeCell<Option<StripGeom>>,
    chain: UnsafeCell<Option<Chain>>,
    nodes: [UnsafeCell<Node>; super::MAX_STRIP_BUFFERS],
    state: Spinlock<State>,
}

unsafe impl Send for Codec {}
unsafe impl Sync for Codec {}

static CODEC: OnceLock<Codec> = OnceLock::new();

pub(super) fn init() -> Result<()> {
    CODEC.get_or_try_init(Codec::new).map(|_| ())
}

fn codec() -> &'static Codec {
    CODEC.get().expect("jpeg codec set up by Decoder::new")
}

impl Codec {
    fn new() -> Result<Self> {
        unsafe { sys::media_engine_jpeg_clock(true) };
        let mut hal: sys::jpeg_hal_context_t = unsafe { mem::zeroed() };
        unsafe { sys::jpeg_hal_init(&mut hal) };
        let config = sys::dma2d_pool_config_t { pool_id: 0, intr_priority: 0 };
        let mut pool = ptr::null_mut();
        if let Err(err) = Error::check(unsafe { sys::dma2d_acquire_pool(&config, &mut pool) }) {
            error!(TAG, "2D-DMA pool: {err}");
            return Err(err);
        }
        Ok(Self {
            lock: Mutex::new(()),
            hal: UnsafeCell::new(hal),
            pool,
            placeholder: dma::internal(unsafe { sys::dma2d_get_trans_elm_size() }, 4)?.cast(),
            trans: UnsafeCell::new(unsafe { mem::zeroed() }),
            tx: Descriptor::new()?,
            rx: [Descriptor::new()?, Descriptor::new()?, Descriptor::new()?],
            setup: UnsafeCell::new(Setup { sampling: Sampling::Gray, csc: 0, scramble: 0, full: None }),
            tables: UnsafeCell::new(Tables::new()),
            geom: UnsafeCell::new(None),
            chain: UnsafeCell::new(None),
            nodes: [UnsafeCell::new(Node::new()), UnsafeCell::new(Node::new())],
            state: Spinlock::new(State::new()),
        })
    }

    fn dev(&self) -> *mut sys::jpeg_dev_t {
        unsafe { (*self.hal.get()).dev }
    }

    /// The codec's interrupt, allocated by the first frame; IDF's driver must
    /// not be using the codec in the same firmware.
    fn interrupt(&'static self) -> Result<()> {
        static ALLOCATED: OnceLock<()> = OnceLock::new();
        ALLOCATED
            .get_or_try_init(|| {
                let dev = self.dev();
                unsafe { sys::jpeg_ll_clear_intr_mask(dev, DECODER_EVENTS) };
                let status = unsafe { sys::jpeg_ll_get_interrupt_status_reg(dev) } as u32;
                let mut handle = ptr::null_mut();
                let err = unsafe {
                    sys::esp_intr_alloc_intrstatus(
                        sys::periph_interrupt_t_ETS_JPEG_INTR_SOURCE as i32,
                        0,
                        status,
                        DECODER_EVENTS,
                        Some(on_interrupt),
                        ptr::from_ref(self).cast_mut().cast(),
                        &mut handle,
                    )
                };
                Error::check(err).inspect_err(|err| error!(TAG, "interrupt: {err}"))?;
                unsafe { sys::jpeg_ll_enable_intr_mask(dev, DECODER_EVENTS & !OUT_EOF) };
                Ok::<(), Error>(())
            })
            .map(|_| ())
    }
}

unsafe extern "C" fn on_interrupt(arg: *mut c_void) {
    let codec: &'static Codec = unsafe { &*arg.cast() };
    let dev = codec.dev();
    let status = unsafe { sys::jpeg_ll_get_intr_status(dev) };
    unsafe { sys::jpeg_ll_clear_intr_mask(dev, status) };
    let errors = status & DECODER_EVENTS & !OUT_EOF;
    if errors != 0 {
        wake(codec, |s| s.status |= errors);
    }
}

fn wake<T>(codec: &'static Codec, f: impl FnOnce(&mut State) -> T) -> T {
    let (out, waker) = codec.state.with(|s| {
        let out = f(s);
        s.events = s.events.wrapping_add(1);
        (out, s.waker.clone())
    });
    if let Some(waker) = waker {
        waker.wake_by_ref();
    }
    out
}

/// Strips delivered but not yet handed to the engine, taken for the caller to
/// hand over; `submitting` stays up until it has.
fn take_ready(s: &mut State) -> Option<(u16, u16)> {
    if !s.chain || s.submitted >= s.delivered {
        return None;
    }
    let range = (s.submitted, s.delivered);
    s.submitted = s.delivered;
    s.submitting += 1;
    Some(range)
}

fn submit(codec: &'static Codec, (from, to): (u16, u16)) {
    let chain = unsafe { (*codec.chain.get()).as_ref() };
    let geom = unsafe { (*codec.geom.get()).as_ref() };
    if let (Some(chain), Some(g)) = (chain, geom) {
        for index in from..to {
            let buffer = g.layout.buffers[usize::from(index) % g.layout.count];
            let plan = chain.plan(buffer, index * g.layout.rows, g.rows_of(index));
            let hook = Hook { f: strip_done, arg: ptr::from_ref(codec).cast(), index };
            let node = codec.nodes[usize::from(index) % g.layout.count].get();
            match plan {
                Ok(Some(plan)) => {
                    if chain.ppa.chain(plan, node, hook).is_err() {
                        unsafe { strip_done(hook.arg, index, false) };
                    }
                }
                Ok(None) => unsafe { strip_done(hook.arg, index, true) },
                Err(_) => unsafe { strip_done(hook.arg, index, false) },
            }
        }
    }
    codec.state.with(|s| s.submitting -= 1);
}

unsafe fn strip_done(arg: *const c_void, index: u16, done: bool) {
    let codec: &'static Codec = unsafe { &*arg.cast() };
    link_after(codec, index);
    wake(codec, |s| {
        s.completed += 1;
        s.failed |= !done;
    });
}

/// Links the strip that reuses strip `index`'s buffer.
fn link_after(codec: &'static Codec, index: u16) {
    let Some(g) = (unsafe { *codec.geom.get() }) else { return };
    let tail = index + g.layout.count as u16;
    if tail >= g.layout.strip_count {
        return;
    }
    let (prev, next) = (&codec.rx[usize::from(tail - 1) % RX_POOL], &codec.rx[usize::from(tail) % RX_POOL]);
    codec.state.with(|s| {
        if !s.active || s.rx.is_null() {
            return;
        }
        unsafe {
            next.write(&g.fields(tail));
            prev.link(Some(next));
            sys::dma2d_append(s.rx);
        }
        s.tail = tail;
    });
}

unsafe extern "C" fn on_desc_done(_: sys::dma2d_channel_handle_t, _: *mut sys::dma2d_event_data_t, arg: *mut c_void) -> bool {
    let codec: &'static Codec = unsafe { &*arg.cast() };
    let ready = wake(codec, |s| {
        if s.active && s.strips && s.delivered < s.strip_count {
            s.delivered += 1;
        }
        take_ready(s)
    });
    if let Some(range) = ready {
        submit(codec, range);
    }
    false
}

unsafe extern "C" fn on_rx_eof(_: sys::dma2d_channel_handle_t, _: *mut sys::dma2d_event_data_t, arg: *mut c_void) -> bool {
    let codec: &'static Codec = unsafe { &*arg.cast() };
    // The last strip's descriptor-done is not always reported, but the end of
    // the frame means every strip is written.
    let ready = wake(codec, |s| {
        s.delivered = s.strip_count;
        s.eof = true;
        take_ready(s)
    });
    if let Some(range) = ready {
        submit(codec, range);
    }
    false
}

fn channels(n: u32, chans: *const sys::dma2d_trans_channel_info_t) -> (sys::dma2d_channel_handle_t, sys::dma2d_channel_handle_t) {
    let (mut tx, mut rx) = (ptr::null_mut(), ptr::null_mut());
    for c in unsafe { core::slice::from_raw_parts(chans, n as usize) } {
        if c.dir == sys::dma2d_channel_direction_t_DMA2D_CHANNEL_DIRECTION_TX {
            tx = c.chan;
        } else {
            rx = c.chan;
        }
    }
    (tx, rx)
}

fn ability(mb_size: u32) -> sys::dma2d_transfer_ability_t {
    sys::dma2d_transfer_ability_t { access_ext_mem: false, data_burst_length: 128, desc_burst_en: true, mb_size }
}

/// Only RX channel 0 converts colour, and IDF's driver loads studio-range
/// matrices only, so this one goes over what
/// `dma2d_configure_color_space_conversion` wrote. Each row is A[9:0] B[20:10]
/// in one word and C[9:0] D[27:10] in the next.
unsafe fn load_matrix(table: &[[i32; 4]; 3]) {
    unsafe {
        let hw = &raw mut sys::DMA2D.in_channel[0].in_color_param_group;
        for (reg, row) in [&raw mut (*hw).param_h, &raw mut (*hw).param_m, &raw mut (*hw).param_l].into_iter().zip(table) {
            let first = (row[0] as u32 & 0x3FF) | (row[1] as u32 & 0x7FF) << 10;
            let second = (row[2] as u32 & 0x3FF) | (row[3] as u32 & 0x3FFFF) << 10;
            ptr::write_volatile(&raw mut (*reg).val[0], first);
            ptr::write_volatile(&raw mut (*reg).val[1], second);
        }
    }
}

unsafe extern "C" fn picked(n: u32, chans: *const sys::dma2d_trans_channel_info_t, arg: *mut c_void) -> bool {
    let codec: &'static Codec = unsafe { &*arg.cast() };
    let setup = unsafe { *codec.setup.get() };
    let (tx, rx) = channels(n, chans);
    codec.state.with(|s| s.rx = rx);
    let mb = match setup.sampling {
        Sampling::Yuv444 | Sampling::Gray => sys::dma2d_macro_block_size_t_DMA2D_MACRO_BLOCK_SIZE_8_8,
        Sampling::Yuv422 => sys::dma2d_macro_block_size_t_DMA2D_MACRO_BLOCK_SIZE_8_16,
        Sampling::Yuv420 => sys::dma2d_macro_block_size_t_DMA2D_MACRO_BLOCK_SIZE_16_16,
    };
    unsafe {
        let mut trig = sys::dma2d_trigger_t {
            periph: sys::dma2d_trigger_peripheral_t_DMA2D_TRIG_PERIPH_JPEG_DECODER,
            periph_sel_id: sys::SOC_DMA2D_TRIG_PERIPH_JPEG_TX as _,
        };
        sys::dma2d_connect(tx, &trig);
        trig.periph_sel_id = sys::SOC_DMA2D_TRIG_PERIPH_JPEG_RX as _;
        sys::dma2d_connect(rx, &trig);
        sys::dma2d_set_transfer_ability(tx, &ability(sys::dma2d_macro_block_size_t_DMA2D_MACRO_BLOCK_SIZE_NONE));
        sys::dma2d_set_transfer_ability(rx, &ability(mb));
        let mut csc: sys::dma2d_csc_config_t = mem::zeroed();
        csc.__bindgen_anon_1.rx_csc_option = setup.csc;
        csc.post_scramble = setup.scramble;
        sys::dma2d_configure_color_space_conversion(rx, &csc);
        if let Some(table) = setup.full {
            load_matrix(table);
        }
        let mut cbs: sys::dma2d_rx_event_callbacks_t = mem::zeroed();
        cbs.on_recv_eof = Some(on_rx_eof);
        cbs.on_desc_done = Some(on_desc_done);
        sys::dma2d_register_rx_event_callbacks(rx, &mut cbs, arg);
        sys::dma2d_set_desc_addr(tx, codec.tx.dma as isize);
        sys::dma2d_set_desc_addr(rx, codec.rx[0].dma as isize);
        sys::dma2d_start(tx);
        sys::dma2d_start(rx);
        sys::jpeg_ll_process_start(codec.dev());
    }
    false
}

fn hb_index(format: ImageFormat, direct: bool) -> usize {
    match (direct, format) {
        (true, _) => 0,
        (_, ImageFormat::Rgb565) => 3,
        _ => 2,
    }
}

fn sampling_index(s: Sampling) -> usize {
    match s {
        Sampling::Yuv444 => 0,
        Sampling::Yuv422 => 1,
        Sampling::Yuv420 => 2,
        Sampling::Gray => 3,
    }
}

pub(super) struct Session {
    header: Header,
    format: ImageFormat,
    setup: Setup,
    hb: u32,
    data: *const u8,
    len: usize,
    output: Output,
    chain: Option<Chain>,
    row_bytes: usize,
    timeout: Duration,
    lock: Option<Pin<Box<MutexLock<'static, ()>>>>,
    guard: Option<MutexGuard<'static, ()>>,
    started: bool,
    finished: bool,
    next: u16,
    seen: u32,
    since: Instant,
    timer: Option<Pin<Box<Timer>>>,
    pub blocking: bool,
}

unsafe impl Send for Session {}

impl Session {
    pub(super) fn new(frame: Frame<'_>) -> Result<Self> {
        let h = &frame.header;
        let format = frame.config.format;
        let direct = h.sampling == Sampling::Gray;
        let hb = HB[sampling_index(h.sampling)][hb_index(format, direct)];
        if hb == 0 {
            return Err(Error::NOT_SUPPORTED);
        }
        let bt601 = frame.config.yuv.standard == YuvStandard::Bt601;
        let (csc, full) = match format {
            ImageFormat::Rgb565 if bt601 => (sys::dma2d_csc_rx_option_t_DMA2D_CSC_RX_YUV420_TO_RGB565_601, &FULL_BT601),
            ImageFormat::Rgb565 => (sys::dma2d_csc_rx_option_t_DMA2D_CSC_RX_YUV420_TO_RGB565_709, &FULL_BT709),
            ImageFormat::Rgb888 | ImageFormat::Bgr888 if bt601 => (sys::dma2d_csc_rx_option_t_DMA2D_CSC_RX_YUV420_TO_RGB888_601, &FULL_BT601),
            ImageFormat::Rgb888 | ImageFormat::Bgr888 => (sys::dma2d_csc_rx_option_t_DMA2D_CSC_RX_YUV420_TO_RGB888_709, &FULL_BT709),
            _ => (sys::dma2d_csc_rx_option_t_DMA2D_CSC_RX_NONE, &FULL_BT601),
        };
        let scramble = if format == ImageFormat::Rgb888 {
            sys::dma2d_scramble_order_t_DMA2D_SCRAMBLE_ORDER_BYTE0_1_2
        } else {
            sys::dma2d_scramble_order_t_DMA2D_SCRAMBLE_ORDER_BYTE2_1_0
        };
        let rgb = format != ImageFormat::L8;
        let full = (rgb && frame.config.yuv.range == YuvRange::Full).then_some(full);
        if let Output::Whole(ptr, len) = frame.output
            && (ptr as usize % 64 != 0 || len % 64 != 0)
        {
            return Err(Error::INVALID_ARG);
        }
        Ok(Self {
            header: frame.header,
            format,
            setup: Setup { sampling: frame.header.sampling, csc, scramble, full },
            hb,
            data: frame.data.as_ptr(),
            len: frame.data.len(),
            output: frame.output,
            chain: frame.chain,
            row_bytes: frame.row_bytes,
            timeout: frame.timeout,
            lock: None,
            guard: None,
            started: false,
            finished: false,
            next: 0,
            seen: 0,
            since: Instant::now(),
            timer: None,
            blocking: false,
        })
    }

    pub(super) fn row_bytes(&self) -> usize {
        self.row_bytes
    }

    pub(super) fn slice(&self) -> Duration {
        self.timeout
    }

    fn strip_layout(&self) -> Option<Layout> {
        match self.output {
            Output::Strips(l) => Some(l),
            Output::Whole(..) => None,
        }
    }

    fn geom(&self, layout: Layout) -> StripGeom {
        StripGeom {
            layout,
            hb: self.hb,
            mcu_h: u32::from(self.header.mcu.1),
            padded_width: u32::from(self.header.padded_width),
            padded_height: self.header.padded_height,
            bits: self.format.bits(),
        }
    }

    fn program(&self, codec: &Codec) -> Result<()> {
        let h = &self.header;
        let t = unsafe { &mut *codec.tables.get() };
        header::parse(unsafe { core::slice::from_raw_parts(self.data, self.len) }, Some(t))?;
        let dev = codec.dev();
        let hal = codec.hal.get();
        unsafe {
            sys::jpeg_ll_soft_rst(dev);
            sys::jpeg_ll_set_codec_mode(dev, sys::jpeg_codec_mode_t_JPEG_CODEC_DECODER);
            for (i, table) in t.quant.iter_mut().enumerate().take(usize::from(t.quant_count)) {
                if let Some(f) = sys::dqt_func[i] {
                    f(dev, table.as_mut_ptr());
                }
            }
            sys::jpeg_ll_set_picture_height(dev, u32::from(h.padded_height));
            sys::jpeg_ll_set_picture_width(dev, u32::from(h.padded_width));
            sys::jpeg_ll_set_decode_component_num(dev, t.component_count);
            for (i, c) in t.components.iter().enumerate().take(usize::from(t.component_count)) {
                if let Some(f) = sys::sof_func[i] {
                    f(dev, c.id, c.h, c.v, c.table);
                }
            }
            let (bits, values) = (&mut t.huff_bits, &mut t.huff_values);
            let mut tmp = [0u32; 162];
            for class in 0..2 {
                for id in 0..2 {
                    if let Some(f) = sys::dht_func[class][id] {
                        f(hal, bits[class][id].as_mut_ptr(), values[class][id].as_mut_ptr(), tmp.as_mut_ptr());
                    }
                }
            }
            sys::jpeg_ll_set_restart_interval(dev, t.restart_interval);
        }
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        let codec = codec();
        codec.interrupt()?;
        self.program(codec)?;
        let scan = self.header.scan;
        let left = (self.len - scan) as u32;
        let input = unsafe { self.data.add(scan) };
        unsafe {
            codec.tx.write(&Fields {
                block: (left & 0x3FFF, left & 0x3FFF),
                picture: (left >> 14, left >> 14),
                at: (0, 0),
                two_d: false,
                multiple: false,
                eof: false,
                pbyte: 1,
                buffer: input,
            });
            let _ = cache::msync(input, left as usize, Flags::C2M | Flags::UNALIGNED);
        }
        let (strips, strip_count, linked) = match self.output {
            Output::Whole(ptr, len) => {
                unsafe {
                    codec.rx[0].write(&Fields {
                        block: (self.hb, u32::from(self.header.mcu.1)),
                        picture: (u32::from(self.header.padded_width), u32::from(self.header.padded_height)),
                        at: (0, 0),
                        two_d: true,
                        multiple: true,
                        eof: false,
                        pbyte: dma::pbyte(self.format.bits()),
                        buffer: ptr,
                    });
                    cache::msync(ptr, len, Flags::C2M | Flags::INVALIDATE)?;
                }
                (false, 1, 1)
            }
            Output::Strips(l) => {
                let g = self.geom(l);
                let linked = l.count.min(usize::from(l.strip_count));
                for i in 0..linked {
                    unsafe { codec.rx[i].write(&g.fields(i as u16)) };
                    if i > 0 {
                        unsafe { codec.rx[i - 1].link(Some(&codec.rx[i])) };
                    }
                }
                for &buffer in &l.buffers[..l.count] {
                    let len = (self.row_bytes * usize::from(l.rows)).next_multiple_of(64);
                    unsafe { cache::msync(buffer, len, Flags::C2M | Flags::INVALIDATE) }?;
                }
                (true, l.strip_count, linked)
            }
        };
        unsafe {
            *codec.setup.get() = self.setup;
            *codec.geom.get() = self.strip_layout().map(|l| self.geom(l));
            *codec.chain.get() = self.chain;
        }
        codec.state.with(|s| {
            *s = State { active: true, strips, strip_count, tail: linked as u16 - 1, chain: self.chain.is_some(), ..State::new() }
        });
        let trans = codec.trans.get();
        unsafe {
            (*trans).tx_channel_num = 1;
            (*trans).rx_channel_num = 1;
            (*trans).channel_flags = sys::DMA2D_CHANNEL_FUNCTION_FLAG_RX_REORDER;
            (*trans).specified_tx_channel_mask = 0;
            (*trans).specified_rx_channel_mask = 0;
            (*trans).on_job_picked = Some(picked);
            (*trans).user_config = ptr::from_ref(codec).cast_mut().cast();
        }
        self.started = true;
        self.since = Instant::now();
        Error::check(unsafe { sys::dma2d_enqueue(codec.pool, trans, codec.placeholder) })
            .inspect_err(|err| error!(TAG, "2D-DMA enqueue: {err}"))
    }

    fn finish(&mut self) {
        if !self.started || self.finished {
            return;
        }
        self.finished = true;
        let codec = codec();
        let eof = codec.state.with(|s| {
            s.chain = false;
            s.active = false;
            s.rx = ptr::null_mut();
            s.waker = None;
            s.eof
        });
        if !eof {
            let mut yield_ = false;
            unsafe { sys::dma2d_force_end(codec.placeholder, &mut yield_) };
        }
        if let Some(chain) = self.chain {
            while codec.state.with(|s| s.submitting) != 0 {
                core::hint::spin_loop();
            }
            for node in &codec.nodes {
                chain.ppa.settle(node.get());
            }
        }
        unsafe {
            *codec.geom.get() = None;
            *codec.chain.get() = None;
        }
        self.guard = None;
    }

    fn fail(&mut self, err: Error) -> Poll<Result<Option<u16>>> {
        self.finish();
        Poll::Ready(Err(err))
    }

    pub(super) fn release(&mut self, index: u16) {
        self.since = Instant::now();
        if self.started && !self.finished {
            link_after(codec(), index);
        }
    }

    fn deliver(&mut self, index: u16) -> Result<()> {
        let Some(l) = self.strip_layout() else { return Ok(()) };
        let len = (self.row_bytes * usize::from(self.geom(l).rows_of(index))).next_multiple_of(64);
        unsafe { cache::msync(l.buffers[usize::from(index) % l.count], len, Flags::M2C) }
    }
}

impl wait::Pollable for Session {
    type Output = Result<Option<u16>>;

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<u16>>> {
        if self.finished {
            return Poll::Ready(Ok(None));
        }
        if self.guard.is_none() {
            let lock = self.lock.get_or_insert_with(|| Box::pin(codec().lock.lock_async()));
            match lock.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(guard) => {
                    self.lock = None;
                    self.guard = Some(guard);
                }
            }
            if let Err(err) = self.start() {
                return self.fail(err);
            }
        }
        let codec = codec();
        let (status, eof, delivered, events, completed, failed) = codec.state.with(|s| {
            s.waker = Some(cx.waker().clone());
            (s.status, s.eof, s.delivered, s.events, s.completed, s.failed)
        });
        if status != 0 {
            error!(TAG, "decoder status {status:#x}");
            return self.fail(Error::INVALID_STATE);
        }
        if failed {
            error!(TAG, "the 2D engine failed a strip");
            return self.fail(Error::FAIL);
        }
        match self.output {
            Output::Strips(l) if self.chain.is_some() => {
                if eof && completed >= l.strip_count {
                    self.finish();
                    return Poll::Ready(Ok(None));
                }
            }
            Output::Whole(ptr, len) if eof => {
                self.finish();
                return Poll::Ready(unsafe { cache::msync(ptr, len, Flags::M2C) }.map(|()| None));
            }
            Output::Strips(_) if self.next < delivered => {
                let index = self.next;
                self.next += 1;
                return Poll::Ready(self.deliver(index).map(|()| Some(index)));
            }
            Output::Strips(l) if eof && self.next >= l.strip_count => {
                self.finish();
                return Poll::Ready(Ok(None));
            }
            _ => {}
        }
        if events != self.seen {
            self.seen = events;
            self.since = Instant::now();
        }
        let idle = self.since.elapsed();
        if idle >= self.timeout {
            error!(TAG, "no progress for {idle:?}, {delivered} strips");
            return self.fail(Error::TIMEOUT);
        }
        if !self.blocking {
            let timer = self.timer.get_or_insert_with(|| Box::pin(idf::executor::sleep(self.timeout - idle)));
            if timer.as_mut().poll(cx).is_ready() {
                self.timer = None;
                cx.waker().wake_by_ref();
            }
        }
        Poll::Pending
    }

    fn forget_waker(&mut self) {
        if let Some(codec) = CODEC.get() {
            codec.state.with(|s| s.waker = None);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.finish();
    }
}
