/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::CStr;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use core::time::Duration;

use idf::heap::{self, Caps};
use idf::log::{error, info, warn};
use idf::sync::{Mutex, Receiver, Semaphore, Sender, channel};
use idf::timer::Timer;
use idf::{Error, Result, thread};

use super::sdio::{self, Sdio};
use super::wire::{self, Header, If};

const TAG: &CStr = c"esp_hosted";

const TX_BUF_NUM: usize = 8;
const CTRL_BUF_NUM: usize = 4;
const CTRL_TOKENS: u32 = 2;
const RX_STREAM_MAX: usize = 32 * 1024;
const RX_BUF_NUM: usize = 4;
const TASK_PRIORITY: u32 = 22;
const TOKEN_POLL: Duration = Duration::from_micros(300);
const INIT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const START_ATTEMPTS: usize = 3;

const S2H_NEW_PACKET: u32 = 1 << 23;
const PKT_LEN_MASK: u32 = 0xFFFFF;
const TOKEN_MASK: u32 = 0xFFF;

const LOG_PREFIX: &[u8] = b"[C6] ";
const LOG_LINE: usize = 256;

pub(super) struct TxBuf(Box<[u8], Caps>);

impl TxBuf {
    pub(super) fn payload(&mut self) -> &mut [u8] {
        &mut self.0[wire::HEADER_LEN..wire::BUF_SIZE]
    }
}

struct Item {
    buf: TxBuf,
    len: usize,
    ctrl: bool,
}

struct Lane {
    free_tx: Sender<TxBuf>,
    free_rx: Receiver<TxBuf>,
    queue_tx: Sender<Item>,
    queue_rx: Receiver<Item>,
}

impl Lane {
    // SDMMC DMA out of PSRAM costs about half again per block written, so the
    // transmit side stays internal; the receive side barely notices.
    fn new(num: usize) -> Result<Self> {
        let (free_tx, free_rx) = channel(num);
        let (queue_tx, queue_rx) = channel(num);
        for _ in 0..num {
            let buf = heap::zeroed_slice(sdio::padded(wire::BUF_SIZE), Caps::INTERNAL | Caps::DMA | Caps::CACHE_ALIGNED)?;
            free_tx.send(TxBuf(buf));
        }
        Ok(Self { free_tx, free_rx, queue_tx, queue_rx })
    }
}

struct Started {
    tx: bool,
    rx: bool,
}

/// A receive buffer whose station frames may be kept past `Receivers::sta`:
/// it is read into again once every `hold` has its `release`.
pub(super) struct RxBuf {
    ptr: *mut u8,
    index: usize,
    holds: AtomicU32,
    free: Sender<usize>,
}

unsafe impl Sync for RxBuf {}

impl RxBuf {
    pub(super) fn hold(&self) {
        self.holds.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn release(&self) {
        if self.holds.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.free.try_send(self.index);
        }
    }
}

/// Where received packets go, on the transport's rx task.
pub(super) trait Receivers: Sync {
    fn serial(&self, payload: &[u8], flags: u8);
    /// `buf` is where `frame` lies, when it may be held past the call.
    fn sta(&self, frame: &[u8], buf: Option<&'static RxBuf>);
    fn hci(&self, payload: &[u8]);
    fn test(&self, payload: &[u8]);
}

pub(super) struct Transport {
    sdio: Sdio,
    receivers: &'static dyn Receivers,
    // Control traffic has its own buffers and goes out first, so a data flood
    // cannot hold back RPC and HCI.
    ctrl: Lane,
    data: Lane,
    pending: Semaphore,
    poll: &'static Semaphore,
    poll_timer: Timer,
    start_lock: Mutex<Started>,
    ready: AtomicBool,
    ready_signal: Semaphore,
    caps: AtomicU8,
    ext_caps: AtomicU32,
    tokens: AtomicU32,
    tx_count: AtomicU32,
    rx_bytes: AtomicU32,
}

impl Transport {
    pub(super) fn new(sdio: Sdio, receivers: &'static dyn Receivers) -> Result<Self> {
        let poll: &'static Semaphore = Box::leak(Box::new(Semaphore::try_new(0, 1)?));
        let poll_timer = Timer::new(c"esp_hosted_poll", || {
            poll.give();
        })?;
        Ok(Self {
            sdio,
            receivers,
            ctrl: Lane::new(CTRL_BUF_NUM)?,
            data: Lane::new(TX_BUF_NUM)?,
            pending: Semaphore::try_new(0, (TX_BUF_NUM + CTRL_BUF_NUM) as u32)?,
            poll,
            poll_timer,
            start_lock: Mutex::new(Started { tx: false, rx: false }),
            ready: AtomicBool::new(false),
            ready_signal: Semaphore::try_new(0, 1)?,
            caps: AtomicU8::new(0),
            ext_caps: AtomicU32::new(0),
            tokens: AtomicU32::new(0),
            tx_count: AtomicU32::new(0),
            rx_bytes: AtomicU32::new(0),
        })
    }

    pub(super) fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// `wire::CAP_*` from the last INIT event.
    pub(super) fn caps(&self) -> u8 {
        self.caps.load(Ordering::Acquire)
    }

    /// `wire::EXT_CAP_*` from the last INIT event; 0 for a stock coprocessor.
    pub(super) fn ext_caps(&self) -> u32 {
        self.ext_caps.load(Ordering::Acquire)
    }

    fn lane(&self, if_type: If) -> &Lane {
        if matches!(if_type, If::Serial | If::Priv | If::Hci) { &self.ctrl } else { &self.data }
    }

    /// `None` on timeout or before the link is up; every buffer got goes back
    /// through `tx_put`.
    pub(super) fn tx_get(&self, if_type: If, wait: Option<Duration>) -> Option<TxBuf> {
        if !self.ready() && if_type != If::Priv {
            return None;
        }
        let free = &self.lane(if_type).free_rx;
        match wait {
            None => Some(free.recv()),
            Some(Duration::ZERO) => free.try_recv(),
            Some(wait) => free.recv_timeout(wait),
        }
    }

    pub(super) fn tx_put(&self, mut buf: TxBuf, if_type: If, flags: u8, pkt_type: u8, len: usize) {
        let header =
            Header { if_type: if_type as u8, flags, len: len as u16, offset: wire::HEADER_LEN as u16, pkt_type };
        header.write(&mut buf.0);
        let ctrl = matches!(if_type, If::Serial | If::Priv | If::Hci);
        self.lane(if_type).queue_tx.send(Item { buf, len: wire::HEADER_LEN + len, ctrl });
        self.pending.give();
    }

    /// Copies `data`; `TIMEOUT` when no buffer frees up within `wait`.
    pub(super) fn send(&self, if_type: If, flags: u8, data: &[u8], wait: Option<Duration>) -> Result<()> {
        if data.len() > wire::MAX_PAYLOAD {
            return Err(Error::INVALID_SIZE);
        }
        if !self.ready() && if_type != If::Priv {
            return Err(Error::INVALID_STATE);
        }
        let mut buf = self.tx_get(if_type, wait).ok_or(Error::TIMEOUT)?;
        buf.payload()[..data.len()].copy_from_slice(data);
        self.tx_put(buf, if_type, flags, 0, data.len());
        Ok(())
    }

    fn have_tokens(&self, need: u32) -> bool {
        let available = |tokens: u32| (tokens.wrapping_sub(self.tx_count.load(Ordering::Relaxed)) & TOKEN_MASK) >= need;
        if available(self.tokens.load(Ordering::Relaxed)) {
            return true;
        }
        let Ok(reg) = self.sdio.read_token() else { return false };
        let tokens = (reg >> 16) & TOKEN_MASK;
        self.tokens.store(tokens, Ordering::Relaxed);
        available(tokens)
    }

    // The slave raises no interrupt when it returns receive buffers, so a short
    // timer paces the token polls instead of a tick-granular delay.
    fn poll_sleep(&self) {
        if self.poll_timer.start_once(TOKEN_POLL).is_ok() {
            self.poll.take();
        } else {
            thread::yield_now();
        }
    }

    fn tx_task(&'static self) {
        let mut held: [Option<Item>; 2] = [None, None];
        loop {
            let ctrl_waiting = peek(&mut held[0], &self.ctrl.queue_rx);
            let data_waiting = peek(&mut held[1], &self.data.queue_rx);
            if !ctrl_waiting && !data_waiting {
                self.pending.take();
                continue;
            }
            // Data leaves the last receive buffers on the slave to control
            // traffic, which can still overtake it while it waits.
            if !ctrl_waiting && !self.have_tokens(1 + CTRL_TOKENS) {
                self.poll_sleep();
                continue;
            }
            let Some(mut item) = pop(&mut held, usize::MAX, &self.ctrl, &self.data) else { continue };
            if self.ext_caps() & wire::EXT_CAP_RX_AGGR != 0 {
                while let Some(next) = pop(&mut held, wire::BUF_SIZE - item.len, &self.ctrl, &self.data) {
                    item.buf.0[item.len..item.len + next.len].copy_from_slice(&next.buf.0[..next.len]);
                    item.len += next.len;
                    self.free(next);
                }
            }
            let need = item.len.div_ceil(wire::BUF_SIZE) as u32;
            while !self.have_tokens(need) {
                self.poll_sleep();
            }
            match unsafe { self.sdio.write(item.buf.0.as_ptr(), item.len) } {
                Ok(()) => {
                    let count = self.tx_count.load(Ordering::Relaxed);
                    self.tx_count.store(count.wrapping_add(need) & TOKEN_MASK, Ordering::Relaxed);
                }
                Err(err) => error!(TAG, "write {}: {err}", item.len),
            }
            self.free(item);
        }
    }

    fn free(&self, item: Item) {
        let lane = if item.ctrl { &self.ctrl } else { &self.data };
        let _ = lane.free_tx.try_send(item.buf);
    }

    fn send_slave_config(&self) {
        const EVENT: [u8; 17] = [
            wire::PRIV_EVENT_INIT, 15,
            wire::TLV_HOST_CAPABILITIES, 1, 0,
            wire::TLV_RCVD_CHIP_ID, 1, wire::CHIP_ID_C6,
            wire::TLV_SLV_TEST_RAW_TP, 1, 0,
            wire::TLV_THROTTLE_HIGH, 1, 80,
            wire::TLV_THROTTLE_LOW, 1, 60,
        ];
        if let Err(err) = self.send(If::Priv, 0, &EVENT, None) {
            error!(TAG, "slave config: {err}");
        }
    }

    fn on_priv(&self, p: &[u8]) {
        if p.len() < 2 || p[0] != wire::PRIV_EVENT_INIT || usize::from(p[1]) > p.len() - 2 {
            return;
        }
        let end = 2 + usize::from(p[1]);
        let mut chip = None;
        let mut caps = None;
        let mut version = 0u32;
        let mut ext = 0u32;
        let mut pos = 2;
        while pos + 2 <= end {
            let (tag, n) = (p[pos], usize::from(p[pos + 1]));
            if pos + 2 + n > end {
                break;
            }
            let v = &p[pos + 2..pos + 2 + n];
            match (tag, n) {
                (wire::TLV_CHIP_ID, 1) => chip = Some(v[0]),
                (wire::TLV_CAPABILITY, 1) => caps = Some(v[0]),
                (wire::TLV_EXT_CAPS, 4) => ext = u32::from_le_bytes([v[0], v[1], v[2], v[3]]),
                (wire::TLV_FW_VERSION, 4) => version = u32::from_le_bytes([v[0], v[1], v[2], 0]),
                _ => {}
            }
            pos += 2 + n;
        }
        info!(
            TAG,
            "coprocessor chip 0x{:02x}, caps 0x{:02x}, ext 0x{ext:02x}, firmware {}.{}.{}",
            chip.unwrap_or(0xff),
            caps.unwrap_or(0xff),
            version >> 16,
            (version >> 8) & 0xff,
            version & 0xff
        );
        self.caps.store(caps.unwrap_or(0), Ordering::Release);
        self.ext_caps.store(ext, Ordering::Release);
        if chip != Some(wire::CHIP_ID_C6) {
            error!(TAG, "unsupported coprocessor chip");
            return;
        }
        self.send_slave_config();
        self.ready.store(true, Ordering::Release);
        self.ready_signal.give();
    }

    fn dispatch_stream(&self, mut p: &[u8], buf: Option<&'static RxBuf>, log: &mut LogLine) {
        while p.len() >= wire::HEADER_LEN {
            let h = Header::read(p);
            let total = usize::from(h.offset) + usize::from(h.len);
            if usize::from(h.offset) != wire::HEADER_LEN
                || h.len == 0
                || usize::from(h.len) > wire::MAX_PAYLOAD
                || total > p.len()
            {
                warn!(TAG, "malformed stream, {} bytes dropped", p.len());
                return;
            }
            let payload = &p[wire::HEADER_LEN..total];
            match If::from_u8(h.if_type) {
                Some(If::Sta) => self.receivers.sta(payload, buf),
                Some(If::Serial) => self.receivers.serial(payload, h.flags),
                Some(If::Priv) => self.on_priv(payload),
                Some(If::Test) => self.receivers.test(payload),
                Some(If::Hci) => self.receivers.hci(payload),
                Some(If::Log) => log.push(payload),
                None => {}
            }
            p = &p[total..];
        }
    }

    // Frames go up still in the buffer they were read into, so there are
    // several; when every one is held, the spare is read into and its frames
    // copied out instead.
    fn rx_task(&'static self, bufs: &'static [RxBuf], free: Receiver<usize>, mut spare: Box<[u8], Caps>) {
        let mut log = LogLine::new();
        loop {
            if self.sdio.wait_int(None).is_err() {
                continue;
            }
            let Ok(st) = self.sdio.read_status() else {
                error!(TAG, "interrupt status read failed");
                continue;
            };
            self.tokens.store((st.token >> 16) & TOKEN_MASK, Ordering::Relaxed);
            let _ = self.sdio.clear_int(st.int_raw);
            if st.int_raw & S2H_NEW_PACKET == 0 {
                continue;
            }
            let rx_bytes = self.rx_bytes.load(Ordering::Relaxed);
            let len = ((st.pkt_len & PKT_LEN_MASK).wrapping_sub(rx_bytes) & PKT_LEN_MASK) as usize;
            if len == 0 {
                continue;
            }
            if len > RX_STREAM_MAX {
                error!(TAG, "stream of {len} bytes exceeds the buffer");
                self.rx_bytes.store(st.pkt_len & PKT_LEN_MASK, Ordering::Relaxed);
                continue;
            }
            let lent = free.try_recv().map(|i| &bufs[i]);
            let ptr = lent.map_or(spare.as_mut_ptr(), |b| b.ptr);
            if let Some(b) = lent {
                b.holds.store(1, Ordering::Relaxed);
            }
            let result = unsafe { self.sdio.read(ptr, len) };
            self.rx_bytes.store((rx_bytes + len as u32) & PKT_LEN_MASK, Ordering::Relaxed);
            match result {
                Ok(()) => self.dispatch_stream(unsafe { core::slice::from_raw_parts(ptr, len) }, lent, &mut log),
                Err(err) => error!(TAG, "read {len}: {err}"),
            }
            if let Some(b) = lent {
                b.release();
            }
        }
    }

    /// Resets the coprocessor and blocks until its INIT event arrived.
    pub(super) fn start(&'static self) -> Result<()> {
        let mut started = self.start_lock.lock();
        if self.ready() {
            return Ok(());
        }
        if !started.tx {
            self.sdio.init()?;
            thread::Builder::new()
                .name("hosted_tx")
                .stack_size(3072)
                .priority(TASK_PRIORITY)
                .spawn(move || self.tx_task())?;
            started.tx = true;
        }
        for _ in 0..START_ATTEMPTS {
            self.rx_bytes.store(0, Ordering::Relaxed);
            self.tx_count.store(0, Ordering::Relaxed);
            self.tokens.store(0, Ordering::Relaxed);
            self.caps.store(0, Ordering::Relaxed);
            self.ext_caps.store(0, Ordering::Relaxed);
            self.ready_signal.try_take();
            if let Err(err) = self.sdio.connect(CONNECT_TIMEOUT) {
                warn!(TAG, "coprocessor not responding: {err}");
                continue;
            }
            if !started.rx {
                let caps = Caps::SPIRAM | Caps::CACHE_ALIGNED;
                let spare = heap::zeroed_slice::<u8>(sdio::padded(RX_STREAM_MAX), caps)?;
                let (free_tx, free_rx) = channel(RX_BUF_NUM);
                let mut bufs = alloc::vec::Vec::with_capacity(RX_BUF_NUM);
                for index in 0..RX_BUF_NUM {
                    let ptr = Box::leak(heap::zeroed_slice::<u8>(sdio::padded(RX_STREAM_MAX), caps)?).as_mut_ptr();
                    bufs.push(RxBuf { ptr, index, holds: AtomicU32::new(0), free: free_tx.clone() });
                    free_tx.send(index);
                }
                let bufs: &'static [RxBuf] = alloc::vec::Vec::leak(bufs);
                thread::Builder::new()
                    .name("hosted_rx")
                    .stack_size(4096)
                    .priority(TASK_PRIORITY)
                    .spawn(move || self.rx_task(bufs, free_rx, spare))?;
                started.rx = true;
            }
            if let Err(err) = self.sdio.notify(wire::H2S_OPEN_DATA_PATH) {
                warn!(TAG, "open data path: {err}");
            }
            if self.ready_signal.take_timeout(INIT_TIMEOUT) {
                return Ok(());
            }
            warn!(TAG, "no INIT event from the coprocessor");
        }
        Err(Error::TIMEOUT)
    }
}

fn peek(slot: &mut Option<Item>, queue: &Receiver<Item>) -> bool {
    if slot.is_none() {
        *slot = queue.try_recv();
    }
    slot.is_some()
}

fn pop(held: &mut [Option<Item>; 2], room: usize, ctrl: &Lane, data: &Lane) -> Option<Item> {
    for (slot, lane) in held.iter_mut().zip([ctrl, data]) {
        if peek(slot, &lane.queue_rx) && slot.as_ref().is_some_and(|item| item.len <= room) {
            return slot.take();
        }
    }
    None
}

// Whole lines only: a partial one left on the console would swallow the next
// line the host itself prints.
struct LogLine {
    buf: [u8; LOG_LINE],
    len: usize,
}

impl LogLine {
    fn new() -> Self {
        let mut buf = [0; LOG_LINE];
        buf[..LOG_PREFIX.len()].copy_from_slice(LOG_PREFIX);
        Self { buf, len: LOG_PREFIX.len() }
    }

    fn push(&mut self, data: &[u8]) {
        for &b in data {
            if self.len < LOG_LINE - 1 {
                self.buf[self.len] = b;
                self.len += 1;
            }
            if b == b'\n' || self.len == LOG_LINE - 1 {
                if b != b'\n' {
                    self.buf[self.len] = b'\n';
                    self.len += 1;
                }
                unsafe { idf::sys::write(1, self.buf.as_ptr().cast(), self.len) };
                self.len = LOG_PREFIX.len();
            }
        }
    }
}
