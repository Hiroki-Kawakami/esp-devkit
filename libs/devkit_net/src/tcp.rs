/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::net::{Ipv4Addr, SocketAddrV4};
use core::time::Duration;

use idf::heap::Caps;

use crate::icmp::Reported;
use crate::ring::Ring;
use crate::stack::{SocketId, Stack, Tx};
use crate::time::Instant;
use crate::wire::{Ipv4, PROTO_TCP, be16, checksum, put16, sum};
use crate::{Error, Result};

const HEADER: usize = 20;
const SYN_OPTIONS: usize = 8;

const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const PSH: u8 = 0x08;
const ACK: u8 = 0x10;

const DEFAULT_MSS: usize = 536;
const RTO_INITIAL: Duration = Duration::from_secs(1);
const RTO_MIN: Duration = Duration::from_millis(200);
const RTO_MAX: Duration = Duration::from_secs(60);
const MAX_RETRIES: u8 = 12;
const MAX_SYN_RETRIES: u8 = 7;
const DELAYED_ACK: Duration = Duration::from_millis(40);
const TIME_WAIT: Duration = Duration::from_secs(60);
const FIN_WAIT_2: Duration = Duration::from_secs(60);
const EPHEMERAL_FIRST: u16 = 49152;
const MAX_WINDOW_SCALE: u8 = 14;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(75);
const KEEPALIVE_PROBES: u8 = 9;

/// SipHash-2-4.
fn siphash(k0: u64, k1: u64, data: &[u8]) -> u64 {
    let mut v = [k0 ^ 0x736f6d6570736575, k1 ^ 0x646f72616e646f6d, k0 ^ 0x6c7967656e657261, k1 ^ 0x7465646279746573];
    let round = |v: &mut [u64; 4]| {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    };
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let m = u64::from_le_bytes(chunk.try_into().unwrap());
        v[3] ^= m;
        round(&mut v);
        round(&mut v);
        v[0] ^= m;
    }
    let mut last = [0u8; 8];
    last[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
    last[7] = data.len() as u8;
    let m = u64::from_le_bytes(last);
    v[3] ^= m;
    round(&mut v);
    round(&mut v);
    v[0] ^= m;
    v[2] ^= 0xff;
    for _ in 0..4 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

/// RFC 3390.
fn initial_window(mss: usize) -> usize {
    (4 * mss).min((2 * mss).max(4380))
}

/// Whether `n` of the `unsent` bytes go out now: RFC 1122 4.2.3.4 (sender
/// silly window avoidance) and Nagle. `forced` is the override timer firing.
pub(crate) fn worth_sending(n: usize, unsent: usize, mss: usize, max_window: usize, in_flight: usize, nodelay: bool, forced: bool) -> bool {
    if n == 0 {
        return false;
    }
    if n >= mss || forced {
        return true;
    }
    if n == unsent {
        return in_flight == 0 || nodelay;
    }
    n * 2 >= max_window
}

fn lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

fn le(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) <= 0
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum State {
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    Closed,
}

impl State {
    fn synchronized(self) -> bool {
        !matches!(self, State::SynSent | State::SynReceived | State::Closed)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Buffers {
    pub rx: usize,
    pub tx: usize,
    pub caps: Caps,
}

/// Out-of-order ranges, as offsets from `rcv_nxt`.
#[derive(Default)]
struct Assembler {
    ranges: [(u32, u32); 4],
    count: usize,
}

impl Assembler {
    /// `false` when the range neither merges nor fits.
    fn add(&mut self, start: u32, end: u32) -> bool {
        let mut merged = (start, end);
        let mut kept = [(0u32, 0u32); 4];
        let mut k = 0;
        for &(s, e) in &self.ranges[..self.count] {
            if e < merged.0 || s > merged.1 {
                kept[k] = (s, e);
                k += 1;
            } else {
                merged = (merged.0.min(s), merged.1.max(e));
            }
        }
        if k == kept.len() {
            return false;
        }
        let pos = kept[..k].iter().position(|&(s, _)| s > merged.0).unwrap_or(k);
        kept.copy_within(pos..k, pos + 1);
        kept[pos] = merged;
        self.ranges = kept;
        self.count = k + 1;
        true
    }

    /// Removes the range at offset 0 and returns its length.
    fn take_front(&mut self) -> u32 {
        if self.count == 0 || self.ranges[0].0 != 0 {
            return 0;
        }
        let n = self.ranges[0].1;
        self.ranges.copy_within(1..self.count, 0);
        self.count -= 1;
        for r in &mut self.ranges[..self.count] {
            *r = (r.0 - n, r.1 - n);
        }
        n
    }
}

struct Segment<'a> {
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    mss: Option<u16>,
    window_scale: Option<u8>,
    data: &'a [u8],
}

impl<'a> Segment<'a> {
    fn parse(ip: &Ipv4, p: &'a [u8]) -> Option<Self> {
        if p.len() < HEADER {
            return None;
        }
        let header = usize::from(p[12] >> 4) * 4;
        if header < HEADER || header > p.len() || checksum(sum(p, pseudo_sum(ip.src, ip.dst, p.len()))) != 0 {
            return None;
        }
        let mut seg = Self {
            src_port: be16(p, 0),
            dst_port: be16(p, 2),
            seq: u32::from_be_bytes(p[4..8].try_into().unwrap()),
            ack: u32::from_be_bytes(p[8..12].try_into().unwrap()),
            flags: p[13],
            window: be16(p, 14),
            mss: None,
            window_scale: None,
            data: &p[header..],
        };
        let mut opts = &p[HEADER..header];
        while let [kind, rest @ ..] = opts {
            match kind {
                0 => break,
                1 => opts = rest,
                _ => {
                    let len = usize::from(*rest.first()?);
                    if len < 2 || len > opts.len() {
                        return None;
                    }
                    match (kind, len) {
                        (2, 4) => seg.mss = Some(be16(opts, 2)),
                        (3, 3) => seg.window_scale = Some(opts[2].min(MAX_WINDOW_SCALE)),
                        _ => {}
                    }
                    opts = &opts[len..];
                }
            }
        }
        Some(seg)
    }

    fn len(&self) -> u32 {
        self.data.len() as u32 + u32::from(self.flags & SYN != 0) + u32::from(self.flags & FIN != 0)
    }
}

fn pseudo_sum(src: Ipv4Addr, dst: Ipv4Addr, len: usize) -> u32 {
    sum(&dst.octets(), sum(&src.octets(), 0)) + u32::from(PROTO_TCP) + len as u32
}

pub(crate) struct Tcb {
    pub state: State,
    local: SocketAddrV4,
    remote: SocketAddrV4,
    listener: Option<usize>,
    released: bool,
    error: Option<Error>,
    /// From an ICMP error; reported if the connection times out.
    soft_error: Option<Error>,
    rx: Ring,
    tx: Ring,
    assembler: Assembler,

    iss: u32,
    snd_una: u32,
    snd_nxt: u32,
    snd_max: u32,
    tx_base: u32,
    snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    rcv_nxt: u32,
    rcv_adv: u32,
    snd_mss: usize,
    rcv_mss: usize,
    snd_scale: u8,
    rcv_scale: u8,
    scaling: bool,
    fin_queued: bool,
    fin_sent: bool,
    fin_seq: Option<u32>,
    fin_received: bool,

    rto: Duration,
    srtt: Option<Duration>,
    rttvar: Duration,
    rtt_sample: Option<(u32, Instant)>,
    retransmit_at: Option<Instant>,
    retries: u8,
    ack_at: Option<Instant>,
    ack_now: bool,
    unacked_segments: u8,
    probe: bool,
    persist_at: Option<Instant>,
    max_window: u32,
    last_sent: Instant,
    keepalive: Option<Duration>,
    heard: Instant,
    keepalive_probes: u8,
    close_at: Option<Instant>,

    cwnd: usize,
    ssthresh: usize,
    dupacks: u8,
    recover: Option<u32>,
    pub nodelay: bool,
}

impl Tcb {
    fn new(
        local: SocketAddrV4,
        remote: SocketAddrV4,
        state: State,
        buffers: Buffers,
        rcv_mss: usize,
        iss: u32,
    ) -> Result<Self> {
        let rcv_scale = (0..=MAX_WINDOW_SCALE).find(|s| (buffers.rx >> s) <= 0xffff).unwrap_or(MAX_WINDOW_SCALE);
        Ok(Self {
            state,
            local,
            remote,
            listener: None,
            released: false,
            error: None,
            soft_error: None,
            rx: Ring::new(buffers.rx, buffers.caps)?,
            tx: Ring::new(buffers.tx, buffers.caps)?,
            assembler: Assembler::default(),
            iss,
            snd_una: iss,
            snd_nxt: iss,
            snd_max: iss,
            tx_base: iss.wrapping_add(1),
            snd_wnd: 0,
            snd_wl1: 0,
            snd_wl2: 0,
            rcv_nxt: 0,
            rcv_adv: 0,
            snd_mss: DEFAULT_MSS,
            rcv_mss,
            snd_scale: 0,
            rcv_scale,
            scaling: false,
            fin_queued: false,
            fin_sent: false,
            fin_seq: None,
            fin_received: false,
            rto: RTO_INITIAL,
            srtt: None,
            rttvar: Duration::ZERO,
            rtt_sample: None,
            retransmit_at: None,
            retries: 0,
            ack_at: None,
            ack_now: false,
            unacked_segments: 0,
            probe: false,
            persist_at: None,
            max_window: 0,
            last_sent: Instant::ZERO,
            keepalive: None,
            heard: Instant::ZERO,
            keepalive_probes: 0,
            close_at: None,
            cwnd: 0,
            ssthresh: usize::MAX,
            dupacks: 0,
            recover: None,
            nodelay: false,
        })
    }

    fn in_flight(&self) -> usize {
        self.snd_nxt.wrapping_sub(self.snd_una) as usize
    }

    fn advance(&mut self, to: u32) {
        self.snd_nxt = to;
        if lt(self.snd_max, to) {
            self.snd_max = to;
        }
    }

    /// Data queued but never sent.
    fn unsent(&self) -> usize {
        if self.fin_sent {
            0
        } else if lt(self.snd_nxt, self.tx_base) {
            self.tx.len()
        } else {
            self.tx.len().saturating_sub(self.snd_nxt.wrapping_sub(self.tx_base) as usize)
        }
    }

    /// What the application may be waiting on.
    fn readiness(&self) -> (State, usize, usize, bool, bool) {
        (self.state, self.rx.len(), self.tx.free(), self.fin_received, self.error.is_some())
    }

    fn on_syn(&mut self, seg: &Segment) {
        self.rcv_nxt = seg.seq.wrapping_add(1);
        self.rcv_adv = self.rcv_nxt;
        self.snd_mss = seg.mss.map_or(DEFAULT_MSS, usize::from).clamp(64, self.rcv_mss.max(64));
        self.scaling = seg.window_scale.is_some();
        match seg.window_scale {
            Some(scale) => self.snd_scale = scale,
            None => self.rcv_scale = 0,
        }
        self.snd_wnd = u32::from(seg.window);
        self.snd_wl1 = seg.seq;
        self.snd_wl2 = seg.ack;
        self.cwnd = initial_window(self.snd_mss);
        self.max_window = self.snd_wnd;
    }

    /// The window to advertise, in bytes, moving the right edge only by a
    /// worthwhile step.
    fn window(&mut self) -> u32 {
        if lt(self.rcv_adv, self.rcv_nxt) {
            self.rcv_adv = self.rcv_nxt;
        }
        let free = (self.rx.free() as u32).min(0xffff << self.rcv_scale);
        let current = self.rcv_adv.wrapping_sub(self.rcv_nxt);
        let step = (self.rx.capacity() as u32 / 2).min(self.rcv_mss as u32);
        if free >= current + step {
            self.rcv_adv = self.rcv_nxt.wrapping_add(free);
            free
        } else {
            current
        }
    }

    fn window_grew(&self) -> bool {
        let current = self.rcv_adv.wrapping_sub(self.rcv_nxt);
        let step = (self.rx.capacity() as u32 / 2).min(self.rcv_mss as u32);
        self.rx.free() as u32 >= current + step
    }

    fn sample_rtt(&mut self, rtt: Duration) {
        match self.srtt {
            None => {
                self.srtt = Some(rtt);
                self.rttvar = rtt / 2;
            }
            Some(srtt) => {
                let delta = if srtt > rtt { srtt - rtt } else { rtt - srtt };
                self.rttvar = (self.rttvar * 3 + delta) / 4;
                self.srtt = Some((srtt * 7 + rtt) / 8);
            }
        }
        self.rto = (self.srtt.unwrap() + (self.rttvar * 4).max(Duration::from_millis(10))).clamp(RTO_MIN, RTO_MAX);
    }

    fn close_with(&mut self, error: Option<Error>) {
        self.state = State::Closed;
        if self.error.is_none() {
            self.error = error;
        }
        self.retransmit_at = None;
        self.ack_at = None;
        self.persist_at = None;
        self.close_at = None;
    }

    /// When to probe an idle connection next, or give it up.
    fn keepalive_at(&self) -> Option<Instant> {
        let idle = self.keepalive?;
        let quiet = matches!(self.state, State::Established | State::CloseWait)
            && self.snd_una == self.snd_max
            && self.unsent() == 0;
        quiet.then(|| self.heard + idle + KEEPALIVE_INTERVAL * u32::from(self.keepalive_probes))
    }

    fn deadline(&self) -> Option<Instant> {
        [self.retransmit_at, self.ack_at, self.persist_at, self.close_at, self.keepalive_at()].into_iter().flatten().min()
    }
}

pub(crate) struct Listener {
    local: SocketAddrV4,
    backlog: usize,
    buffers: Buffers,
    ready: VecDeque<usize>,
}

#[derive(Default)]
pub(crate) struct Sockets {
    tcbs: Vec<Option<Tcb>>,
    listeners: Vec<Option<Listener>>,
    isn_key: Option<(u64, u64)>,
}

impl Sockets {
    fn port_in_use(&self, port: u16) -> bool {
        self.listeners.iter().flatten().any(|l| l.local.port() == port)
            || self.tcbs.iter().flatten().any(|t| t.local.port() == port)
    }

    /// RFC 6056: from a random point of the range.
    fn ephemeral(&mut self) -> Result<u16> {
        let span = u32::from(u16::MAX - EPHEMERAL_FIRST) + 1;
        let start = idf::system::random() % span;
        (0..span)
            .map(|i| EPHEMERAL_FIRST + ((start + i) % span) as u16)
            .find(|&port| !self.port_in_use(port))
            .ok_or(Error::AddrInUse)
    }

    /// RFC 6528: a 4 µs clock plus a keyed hash of the connection.
    pub(crate) fn isn(&mut self, local: SocketAddrV4, remote: SocketAddrV4, now: Instant) -> u32 {
        let (k0, k1) = *self.isn_key.get_or_insert_with(|| {
            let word = || u64::from(idf::system::random()) << 32 | u64::from(idf::system::random());
            (word(), word())
        });
        let mut tuple = [0u8; 12];
        tuple[..4].copy_from_slice(&local.ip().octets());
        tuple[4..6].copy_from_slice(&local.port().to_be_bytes());
        tuple[6..10].copy_from_slice(&remote.ip().octets());
        tuple[10..].copy_from_slice(&remote.port().to_be_bytes());
        ((now.as_micros() >> 2) as u32).wrapping_add(siphash(k0, k1, &tuple) as u32)
    }

    fn insert(&mut self, tcb: Tcb) -> usize {
        match self.tcbs.iter().position(Option::is_none) {
            Some(id) => {
                self.tcbs[id] = Some(tcb);
                id
            }
            None => {
                self.tcbs.push(Some(tcb));
                self.tcbs.len() - 1
            }
        }
    }

    fn tcb(&mut self, id: usize) -> &mut Tcb {
        self.tcbs[id].as_mut().unwrap()
    }
}

impl Stack {
    fn wake(&mut self, socket: SocketId) {
        if !self.woken.contains(&socket) {
            self.woken.push(socket);
        }
    }

    /// Runs `f` on the connection taken out of its slot, so `f` may send.
    fn with_tcb<R>(&mut self, id: usize, f: impl FnOnce(&mut Stack, &mut Tcb) -> R) -> R {
        let mut tcb = self.tcp.tcbs[id].take().unwrap();
        let before = tcb.readiness();
        let result = f(self, &mut tcb);
        let changed = tcb.readiness() != before;
        if tcb.state == State::TimeWait && tcb.released && tcb.rx.capacity() + tcb.tx.capacity() > 0 {
            tcb.rx = Ring::new(0, Caps::DEFAULT).unwrap();
            tcb.tx = Ring::new(0, Caps::DEFAULT).unwrap();
        }
        let free = tcb.state == State::Closed && tcb.released;
        let listener = tcb.listener;
        if !free {
            self.tcp.tcbs[id] = Some(tcb);
        } else if let Some(l) = listener.and_then(|l| self.tcp.listeners[l].as_mut()) {
            l.ready.retain(|&r| r != id);
        }
        if changed && !free {
            self.wake(SocketId::Tcp(id));
        }
        result
    }

    fn source_for(&self, dst: Ipv4Addr) -> Option<Ipv4Addr> {
        if dst.is_loopback() || self.is_own_address(dst) {
            return Some(dst);
        }
        let (id, _) = self.route_v4(dst)?;
        self.ipv4(id).map(|c| c.address)
    }

    pub(crate) fn tcp_listen(&mut self, addr: SocketAddrV4, backlog: usize, buffers: Buffers) -> Result<usize> {
        if !addr.ip().is_unspecified() && !self.is_own_address(*addr.ip()) {
            return Err(Error::AddrNotAvailable);
        }
        let port = match addr.port() {
            0 => self.tcp.ephemeral()?,
            port if self.tcp.listeners.iter().flatten().any(|l| l.local.port() == port) => return Err(Error::AddrInUse),
            port => port,
        };
        let listener =
            Listener { local: SocketAddrV4::new(*addr.ip(), port), backlog: backlog.max(1), buffers, ready: VecDeque::new() };
        let slots = &mut self.tcp.listeners;
        match slots.iter().position(Option::is_none) {
            Some(id) => {
                slots[id] = Some(listener);
                Ok(id)
            }
            None => {
                slots.push(Some(listener));
                Ok(slots.len() - 1)
            }
        }
    }

    pub(crate) fn tcp_listener_addr(&self, id: usize) -> SocketAddrV4 {
        self.tcp.listeners[id].as_ref().unwrap().local
    }

    pub(crate) fn tcp_accept(&mut self, id: usize) -> Option<usize> {
        let conn = self.tcp.listeners[id].as_mut().unwrap().ready.pop_front()?;
        self.tcp.tcb(conn).listener = None;
        Some(conn)
    }

    pub(crate) fn tcp_unlisten(&mut self, id: usize, now: Instant, tx: &mut Tx) {
        self.tcp.listeners[id] = None;
        for conn in 0..self.tcp.tcbs.len() {
            if self.tcp.tcbs[conn].as_ref().is_some_and(|t| t.listener == Some(id)) {
                self.with_tcb(conn, |stack, tcb| {
                    tcb.released = true;
                    tcb.listener = None;
                    stack.tcp_abort(tcb, now, tx);
                });
            }
        }
        self.woken.retain(|w| *w != SocketId::Listener(id));
    }

    pub(crate) fn tcp_connect(&mut self, remote: SocketAddrV4, buffers: Buffers, now: Instant, tx: &mut Tx) -> Result<usize> {
        self.tcp_connect_from(None, remote, buffers, now, tx)
    }

    /// `local_port` `None` picks a free one.
    pub(crate) fn tcp_connect_from(
        &mut self,
        local_port: Option<u16>,
        remote: SocketAddrV4,
        buffers: Buffers,
        now: Instant,
        tx: &mut Tx,
    ) -> Result<usize> {
        let src = self.source_for(*remote.ip()).ok_or(Error::NoRoute)?;
        let mss = self.mtu_to(*remote.ip()).ok_or(Error::NoRoute)? - HEADER;
        let local = SocketAddrV4::new(src, match local_port {
            Some(port) => port,
            None => self.tcp.ephemeral()?,
        });
        let iss = self.tcp.isn(local, remote, now);
        let mut tcb = Tcb::new(local, remote, State::SynSent, buffers, mss, iss)?;
        tcb.cwnd = mss;
        let id = self.tcp.insert(tcb);
        self.with_tcb(id, |stack, tcb| stack.tcp_output(tcb, now, tx));
        Ok(id)
    }

    /// `None` while the handshake runs.
    pub(crate) fn tcp_connected(&mut self, id: usize) -> Option<Result<()>> {
        let tcb = self.tcp.tcb(id);
        match tcb.state {
            State::SynSent | State::SynReceived => None,
            State::Closed => Some(Err(tcb.error.unwrap_or(Error::NotConnected))),
            _ => Some(Ok(())),
        }
    }

    #[cfg(test)]
    pub(crate) fn tcp_debug(&self, id: usize) -> alloc::string::String {
        let t = self.tcp.tcbs[id].as_ref().unwrap();
        alloc::format!(
            "{:?} una={} nxt={} base={} txlen={} unsent={} wnd={} cwnd={} ssth={} rto={:?} rtx={:?} retries={} dup={} rec={:?} rcv_nxt={} rxlen={} asm={:?}/{} ack_at={:?} persist={:?}",
            t.state, t.snd_una.wrapping_sub(t.iss), t.snd_nxt.wrapping_sub(t.iss), t.tx_base.wrapping_sub(t.iss), t.tx.len(), t.unsent(),
            t.snd_wnd, t.cwnd, t.ssthresh, t.rto, t.retransmit_at, t.retries, t.dupacks, t.recover.map(|r| r.wrapping_sub(t.iss)),
            t.rcv_nxt, t.rx.len(), t.assembler.ranges, t.assembler.count, t.ack_at, t.persist_at
        )
    }

    #[cfg(test)]
    pub(crate) fn tcp_state(&self, id: usize) -> Option<State> {
        self.tcp.tcbs.get(id).and_then(Option::as_ref).map(|t| t.state)
    }

    pub(crate) fn tcp_addrs(&mut self, id: usize) -> (SocketAddrV4, SocketAddrV4) {
        let tcb = self.tcp.tcb(id);
        (tcb.local, tcb.remote)
    }

    pub(crate) fn tcp_set_nodelay(&mut self, id: usize, nodelay: bool) {
        self.tcp.tcb(id).nodelay = nodelay;
    }

    pub(crate) fn tcp_nodelay(&mut self, id: usize) -> bool {
        self.tcp.tcb(id).nodelay
    }

    pub(crate) fn tcp_set_keepalive(&mut self, id: usize, idle: Option<Duration>) {
        let tcb = self.tcp.tcb(id);
        tcb.keepalive = idle;
        tcb.keepalive_probes = 0;
    }

    pub(crate) fn tcp_keepalive(&mut self, id: usize) -> Option<Duration> {
        self.tcp.tcb(id).keepalive
    }

    /// `None` when nothing can be read yet; `Ok(0)` at the end of the stream.
    pub(crate) fn tcp_read(&mut self, id: usize, buf: &mut [u8], now: Instant, tx: &mut Tx) -> Option<Result<usize>> {
        self.with_tcb(id, |stack, tcb| {
            if tcb.rx.len() > 0 {
                let n = tcb.rx.pop(buf.len().min(tcb.rx.len()), buf);
                if tcb.state.synchronized() && tcb.window_grew() {
                    tcb.ack_now = true;
                    stack.tcp_output(tcb, now, tx);
                }
                return Some(Ok(n));
            }
            if tcb.fin_received {
                return Some(Ok(0));
            }
            match tcb.state {
                State::Closed => Some(tcb.error.map_or(Ok(0), Err)),
                _ => None,
            }
        })
    }

    /// `None` when the send buffer is full.
    pub(crate) fn tcp_write(&mut self, id: usize, data: &[u8], now: Instant, tx: &mut Tx) -> Option<Result<usize>> {
        self.with_tcb(id, |stack, tcb| {
            if let Some(error) = tcb.error {
                return Some(Err(error));
            }
            if tcb.fin_queued || !matches!(tcb.state, State::Established | State::CloseWait) {
                return Some(Err(Error::BrokenPipe));
            }
            let n = tcb.tx.push(data);
            if n == 0 {
                return None;
            }
            stack.tcp_output(tcb, now, tx);
            Some(Ok(n))
        })
    }

    /// Sends FIN once the queued data has gone.
    pub(crate) fn tcp_shutdown(&mut self, id: usize, now: Instant, tx: &mut Tx) {
        self.with_tcb(id, |stack, tcb| stack.tcp_shutdown_tcb(tcb, now, tx));
    }

    fn tcp_shutdown_tcb(&mut self, tcb: &mut Tcb, now: Instant, tx: &mut Tx) {
        if tcb.fin_queued {
            return;
        }
        match tcb.state {
            State::SynSent => {
                tcb.close_with(None);
                return;
            }
            State::SynReceived | State::Established => tcb.state = State::FinWait1,
            State::CloseWait => tcb.state = State::LastAck,
            _ => return,
        }
        tcb.fin_queued = true;
        self.tcp_output(tcb, now, tx);
    }

    /// The application lets go of the connection; it closes in the background,
    /// or with a reset if received data was left unread.
    pub(crate) fn tcp_release(&mut self, id: usize, now: Instant, tx: &mut Tx) {
        self.with_tcb(id, |stack, tcb| {
            tcb.released = true;
            if tcb.rx.len() > 0 || tcb.state == State::SynSent {
                stack.tcp_abort(tcb, now, tx);
                return;
            }
            stack.tcp_shutdown_tcb(tcb, now, tx);
            if tcb.state == State::FinWait2 && tcb.close_at.is_none() {
                tcb.close_at = Some(now + FIN_WAIT_2);
            }
        });
        self.woken.retain(|w| *w != SocketId::Tcp(id));
    }

    fn tcp_abort(&mut self, tcb: &mut Tcb, now: Instant, tx: &mut Tx) {
        if tcb.state.synchronized() || tcb.state == State::SynReceived {
            self.send_tcp(tcb, tcb.snd_nxt, RST | ACK, 0, 0, now, tx);
        }
        tcb.close_with(None);
    }

    pub(crate) fn tcp_input(&mut self, ip: &Ipv4, unicast: bool, p: &[u8], now: Instant, tx: &mut Tx) {
        if !unicast {
            return;
        }
        let Some(seg) = Segment::parse(ip, p) else { return };
        let local = SocketAddrV4::new(ip.dst, seg.dst_port);
        let remote = SocketAddrV4::new(ip.src, seg.src_port);
        let found = self.tcp.tcbs.iter().position(|t| {
            t.as_ref().is_some_and(|t| t.state != State::Closed && t.local == local && t.remote == remote)
        });
        if let Some(id) = found {
            self.with_tcb(id, |stack, tcb| stack.tcp_process(id, tcb, &seg, now, tx));
            return;
        }
        if seg.flags & (SYN | ACK | RST) == SYN
            && let Some(l) = self.tcp.listeners.iter().position(|l| {
                l.as_ref().is_some_and(|l| {
                    l.local.port() == seg.dst_port && (l.local.ip().is_unspecified() || *l.local.ip() == ip.dst)
                })
            })
        {
            self.tcp_passive_open(l, local, remote, &seg, now, tx);
            return;
        }
        if seg.flags & RST == 0 {
            self.send_reset(local, remote, &seg, now, tx);
        }
    }

    fn tcp_passive_open(
        &mut self,
        l: usize,
        local: SocketAddrV4,
        remote: SocketAddrV4,
        seg: &Segment,
        now: Instant,
        tx: &mut Tx,
    ) {
        let listener = self.tcp.listeners[l].as_ref().unwrap();
        let pending = self.tcp.tcbs.iter().flatten().filter(|t| t.listener == Some(l)).count();
        if pending >= listener.backlog {
            return;
        }
        let buffers = listener.buffers;
        let Some(mss) = self.mtu_to(*remote.ip()) else { return };
        let iss = self.tcp.isn(local, remote, now);
        let Ok(mut tcb) = Tcb::new(local, remote, State::SynReceived, buffers, mss - HEADER, iss) else { return };
        tcb.listener = Some(l);
        tcb.on_syn(seg);
        let id = self.tcp.insert(tcb);
        self.with_tcb(id, |stack, tcb| stack.tcp_output(tcb, now, tx));
    }

    /// RFC 5927: only for a sequence number in flight. Nobody listening ends a
    /// handshake; anything else waits for the connection's own timeout.
    pub(crate) fn tcp_icmp_error(
        &mut self,
        local: SocketAddrV4,
        remote: SocketAddrV4,
        seq: u32,
        reported: Reported,
        _now: Instant,
        _tx: &mut Tx,
    ) {
        let Some(id) = self.tcp.tcbs.iter().position(|t| {
            t.as_ref().is_some_and(|t| t.state != State::Closed && t.local == local && t.remote == remote)
        }) else {
            return;
        };
        self.with_tcb(id, |_, tcb| {
            if !(le(tcb.snd_una, seq) && lt(seq, tcb.snd_max)) {
                return;
            }
            if tcb.state == State::SynSent && reported == Reported::Refused {
                tcb.close_with(Some(Error::ConnectionRefused));
            } else {
                tcb.soft_error = Some(reported.into());
            }
        });
    }

    fn send_reset(&mut self, local: SocketAddrV4, remote: SocketAddrV4, seg: &Segment, now: Instant, tx: &mut Tx) {
        let (seq, ack, flags) =
            if seg.flags & ACK != 0 { (seg.ack, 0, RST) } else { (0, seg.seq.wrapping_add(seg.len()), RST | ACK) };
        let _ = self.send_ipv4(Some(*local.ip()), *remote.ip(), PROTO_TCP, HEADER, now, tx, |out, src| {
            write_header(out, local.port(), remote.port(), seq, ack, flags, 0, HEADER);
            finish(out, src, *remote.ip());
        });
    }

    fn tcp_process(&mut self, id: usize, tcb: &mut Tcb, seg: &Segment, now: Instant, tx: &mut Tx) {
        tcb.heard = now;
        tcb.keepalive_probes = 0;
        if tcb.state == State::SynSent {
            self.tcp_syn_sent(tcb, seg, now, tx);
            return;
        }
        let trimmed;
        let seg = if tcb.state == State::SynReceived
            && seg.flags & SYN != 0
            && seg.seq.wrapping_add(1) == tcb.rcv_nxt
            && tcb.listener.is_none()
        {
            trimmed = Segment { seq: tcb.rcv_nxt, flags: seg.flags & !SYN, ..*seg };
            &trimmed
        } else {
            seg
        };

        let window = tcb.rx.free() as u32;
        let len = seg.len();
        let in_window = |seq: u32| le(tcb.rcv_nxt, seq) && lt(seq, tcb.rcv_nxt.wrapping_add(window));
        let acceptable = match (len, window) {
            (0, 0) => seg.seq == tcb.rcv_nxt,
            (0, _) => in_window(seg.seq),
            (_, 0) => false,
            _ => in_window(seg.seq) || in_window(seg.seq.wrapping_add(len - 1)),
        };
        if !acceptable {
            if seg.flags & RST == 0 {
                tcb.ack_now = true;
                self.tcp_output(tcb, now, tx);
            }
            return;
        }
        if seg.flags & RST != 0 {
            if seg.seq != tcb.rcv_nxt {
                tcb.ack_now = true;
                self.tcp_output(tcb, now, tx);
                return;
            }
            let error = match tcb.state {
                State::SynReceived => {
                    tcb.released |= tcb.listener.is_some();
                    None
                }
                State::Established | State::FinWait1 | State::FinWait2 | State::CloseWait => Some(Error::ConnectionReset),
                _ => None,
            };
            tcb.close_with(error);
            return;
        }
        if seg.flags & SYN != 0 {
            tcb.ack_now = true;
            self.tcp_output(tcb, now, tx);
            return;
        }
        if seg.flags & ACK == 0 {
            return;
        }

        if tcb.state == State::SynReceived {
            if !(lt(tcb.snd_una, seg.ack) && le(seg.ack, tcb.snd_max)) {
                self.send_reset(tcb.local, tcb.remote, seg, now, tx);
                return;
            }
            tcb.state = State::Established;
            tcb.snd_una = seg.ack;
            tcb.snd_wnd = u32::from(seg.window) << tcb.snd_scale;
            tcb.max_window = tcb.max_window.max(tcb.snd_wnd);
            tcb.snd_wl1 = seg.seq;
            tcb.snd_wl2 = seg.ack;
            tcb.retransmit_at = None;
            tcb.retries = 0;
            if let Some((_, sent)) = tcb.rtt_sample.take() {
                tcb.sample_rtt(now - sent);
            }
            if let Some(l) = tcb.listener {
                self.tcp.listeners[l].as_mut().unwrap().ready.push_back(id);
                self.wake(SocketId::Listener(l));
            }
        }

        if lt(tcb.snd_max, seg.ack) || lt(seg.ack, tcb.snd_una.wrapping_sub(tcb.max_window)) {
            tcb.ack_now = true;
            self.tcp_output(tcb, now, tx);
            return;
        }
        let window_update = u32::from(seg.window) << tcb.snd_scale;
        if window_update == 0 {
            tcb.retries = 0;
        }
        if lt(tcb.snd_una, seg.ack) {
            self.tcp_acked(tcb, seg.ack, now, tx);
        } else if seg.ack == tcb.snd_una
            && seg.data.is_empty()
            && seg.flags & FIN == 0
            && window_update == tcb.snd_wnd
            && tcb.snd_una != tcb.snd_max
        {
            tcb.dupacks = tcb.dupacks.saturating_add(1);
            if tcb.dupacks == 3 && tcb.recover.is_none() {
                tcb.ssthresh = (tcb.in_flight() / 2).max(2 * tcb.snd_mss);
                tcb.recover = Some(tcb.snd_nxt);
                self.retransmit_first(tcb, now, tx);
                tcb.cwnd = tcb.ssthresh + 3 * tcb.snd_mss;
            } else if tcb.dupacks > 3 && tcb.recover.is_some() {
                tcb.cwnd += tcb.snd_mss;
            }
        }
        if lt(tcb.snd_wl1, seg.seq) || (tcb.snd_wl1 == seg.seq && le(tcb.snd_wl2, seg.ack)) {
            tcb.snd_wnd = window_update;
            tcb.max_window = tcb.max_window.max(window_update);
            tcb.snd_wl1 = seg.seq;
            tcb.snd_wl2 = seg.ack;
            if tcb.snd_wnd > 0 {
                tcb.persist_at = None;
            }
        }

        if !seg.data.is_empty() && matches!(tcb.state, State::Established | State::FinWait1 | State::FinWait2) {
            self.tcp_data(tcb, seg, now);
        }
        if seg.flags & FIN != 0 && seg.seq.wrapping_add(seg.data.len() as u32) == tcb.rcv_nxt && !tcb.fin_received {
            tcb.rcv_nxt = tcb.rcv_nxt.wrapping_add(1);
            tcb.fin_received = true;
            tcb.ack_now = true;
            let fin_acked = tcb.fin_seq.is_some_and(|f| lt(f, tcb.snd_una));
            tcb.state = match tcb.state {
                State::Established => State::CloseWait,
                State::FinWait1 if fin_acked => State::TimeWait,
                State::FinWait1 => State::Closing,
                State::FinWait2 => State::TimeWait,
                other => other,
            };
        }
        if tcb.state == State::TimeWait {
            tcb.close_at = Some(now + TIME_WAIT);
            tcb.retransmit_at = None;
        }
        self.tcp_output(tcb, now, tx);
    }

    fn tcp_syn_sent(&mut self, tcb: &mut Tcb, seg: &Segment, now: Instant, tx: &mut Tx) {
        let ack_ok = lt(tcb.snd_una, seg.ack) && le(seg.ack, tcb.snd_max);
        if seg.flags & ACK != 0 && !ack_ok {
            if seg.flags & RST == 0 {
                self.send_reset(tcb.local, tcb.remote, seg, now, tx);
            }
            return;
        }
        if seg.flags & RST != 0 {
            if seg.flags & ACK != 0 {
                tcb.close_with(Some(Error::ConnectionRefused));
            }
            return;
        }
        if seg.flags & SYN == 0 {
            return;
        }
        if seg.flags & ACK == 0 {
            tcb.on_syn(seg);
            tcb.state = State::SynReceived;
            tcb.snd_nxt = tcb.iss;
            self.tcp_output(tcb, now, tx);
            return;
        }
        tcb.on_syn(seg);
        tcb.snd_una = seg.ack;
        tcb.state = State::Established;
        tcb.retransmit_at = None;
        tcb.retries = 0;
        if let Some((_, sent)) = tcb.rtt_sample.take() {
            tcb.sample_rtt(now - sent);
        }
        tcb.ack_now = true;
        self.tcp_output(tcb, now, tx);
    }

    fn tcp_acked(&mut self, tcb: &mut Tcb, ack: u32, now: Instant, tx: &mut Tx) {
        let acked = ack.wrapping_sub(tcb.snd_una) as usize;
        if lt(tcb.tx_base, ack) {
            let data = (ack.wrapping_sub(tcb.tx_base) as usize).min(tcb.tx.len());
            tcb.tx.discard(data);
            tcb.tx_base = tcb.tx_base.wrapping_add(data as u32);
        }
        let fin_acked = tcb.fin_seq.is_some_and(|f| lt(f, ack));
        tcb.snd_una = ack;
        if lt(tcb.snd_nxt, ack) {
            tcb.snd_nxt = ack;
        }
        if fin_acked {
            tcb.fin_sent = true;
        }
        if let Some((seq, sent)) = tcb.rtt_sample
            && lt(seq, ack)
        {
            tcb.sample_rtt(now - sent);
            tcb.rtt_sample = None;
        }
        tcb.retries = 0;
        tcb.retransmit_at = (tcb.snd_una != tcb.snd_max).then(|| now + tcb.rto);
        tcb.persist_at = None;
        tcb.dupacks = 0;
        match tcb.recover {
            Some(recover) if le(recover, ack) => {
                tcb.cwnd = tcb.ssthresh;
                tcb.recover = None;
            }
            Some(_) => {
                self.retransmit_first(tcb, now, tx);
                tcb.cwnd = tcb.cwnd.saturating_sub(acked) + tcb.snd_mss;
            }
            None if tcb.cwnd < tcb.ssthresh => tcb.cwnd += acked.min(tcb.snd_mss),
            None => tcb.cwnd += (tcb.snd_mss * tcb.snd_mss / tcb.cwnd.max(1)).max(1),
        }
        if fin_acked {
            match tcb.state {
                State::FinWait1 => {
                    tcb.state = State::FinWait2;
                    if tcb.released {
                        tcb.close_at = Some(now + FIN_WAIT_2);
                    }
                }
                State::Closing => tcb.state = State::TimeWait,
                State::LastAck => tcb.close_with(None),
                _ => {}
            }
        }
    }

    fn tcp_data(&mut self, tcb: &mut Tcb, seg: &Segment, now: Instant) {
        let offset = seg.seq.wrapping_sub(tcb.rcv_nxt) as i32;
        let skip = (-offset).max(0) as usize;
        if skip >= seg.data.len() {
            tcb.ack_now = true;
            return;
        }
        let data = &seg.data[skip..];
        let start = offset.max(0) as usize;
        let end = (start + data.len()).min(tcb.rx.free());
        if start < end {
            tcb.rx.write_free(start, &data[..end - start]);
            tcb.assembler.add(start as u32, end as u32);
        }
        let n = tcb.assembler.take_front();
        if n > 0 {
            tcb.rx.commit(n as usize);
            tcb.rcv_nxt = tcb.rcv_nxt.wrapping_add(n);
        }
        if offset > 0 || n == 0 || tcb.assembler.count > 0 {
            tcb.ack_now = true;
            return;
        }
        tcb.unacked_segments += 1;
        if tcb.unacked_segments >= 2 {
            tcb.ack_now = true;
        } else if tcb.ack_at.is_none() {
            tcb.ack_at = Some(now + DELAYED_ACK);
        }
    }

    fn retransmit_first(&mut self, tcb: &mut Tcb, now: Instant, tx: &mut Tx) {
        let offset = tcb.snd_una.wrapping_sub(tcb.tx_base) as usize;
        let n = tcb.tx.len().saturating_sub(offset).min(tcb.snd_mss);
        if n > 0 {
            self.send_tcp(tcb, tcb.snd_una, ACK, offset, n, now, tx);
            tcb.rtt_sample = None;
        }
    }

    fn tcp_output(&mut self, tcb: &mut Tcb, now: Instant, tx: &mut Tx) {
        match tcb.state {
            State::Closed => return,
            State::SynSent | State::SynReceived => {
                if tcb.snd_nxt == tcb.iss {
                    let flags = if tcb.state == State::SynSent { SYN } else { SYN | ACK };
                    self.send_tcp(tcb, tcb.iss, flags, 0, 0, now, tx);
                    tcb.advance(tcb.iss.wrapping_add(1));
                    if tcb.retries == 0 {
                        tcb.rtt_sample = Some((tcb.iss, now));
                    }
                    tcb.retransmit_at.get_or_insert(now + tcb.rto);
                }
                return;
            }
            _ => {}
        }

        if tcb.in_flight() == 0 && tcb.unsent() > 0 && now - tcb.last_sent > tcb.rto {
            tcb.cwnd = tcb.cwnd.min(initial_window(tcb.snd_mss));
        }
        while !tcb.fin_sent {
            let unsent = tcb.unsent();
            let offset = tcb.snd_nxt.wrapping_sub(tcb.tx_base) as usize;
            let in_flight = tcb.in_flight();
            let window = (tcb.snd_wnd as usize).min(tcb.cwnd).saturating_sub(in_flight);
            let allowed = if tcb.probe { window.max(1) } else { window };
            let n = unsent.min(tcb.snd_mss).min(allowed);
            let fin = tcb.fin_queued && n == unsent;
            let max_window = tcb.max_window as usize;
            if !fin && !worth_sending(n, unsent, tcb.snd_mss, max_window, in_flight, tcb.nodelay, tcb.probe) {
                break;
            }
            let mut flags = ACK;
            if n > 0 && n == unsent {
                flags |= PSH;
            }
            if fin {
                flags |= FIN;
            }
            self.send_tcp(tcb, tcb.snd_nxt, flags, offset, n, now, tx);
            if tcb.rtt_sample.is_none() && n > 0 {
                tcb.rtt_sample = Some((tcb.snd_nxt, now));
            }
            if fin {
                tcb.fin_seq = Some(tcb.snd_nxt.wrapping_add(n as u32));
            }
            tcb.advance(tcb.snd_nxt.wrapping_add(n as u32 + u32::from(fin)));
            tcb.fin_sent |= fin;
            tcb.probe = false;
            tcb.retransmit_at.get_or_insert(now + tcb.rto);
        }

        if tcb.ack_now {
            self.send_tcp(tcb, tcb.snd_nxt, ACK, 0, 0, now, tx);
        }
        if tcb.unsent() > 0 && tcb.in_flight() == 0 && tcb.persist_at.is_none() {
            tcb.persist_at = Some(now + tcb.rto);
        }
    }

    fn send_tcp(&mut self, tcb: &mut Tcb, seq: u32, flags: u8, offset: usize, len: usize, now: Instant, tx: &mut Tx) {
        let syn = flags & SYN != 0;
        let header = if syn { HEADER + SYN_OPTIONS } else { HEADER };
        let window = if syn {
            tcb.window().min(0xffff)
        } else {
            tcb.window().min(0xffff << tcb.rcv_scale) >> tcb.rcv_scale
        };
        let ack = if flags & ACK != 0 { tcb.rcv_nxt } else { 0 };
        let (local, remote) = (tcb.local, tcb.remote);
        let (mss, scale) = (tcb.rcv_mss, tcb.rcv_scale);
        let offer_scale = tcb.state == State::SynSent || tcb.scaling;
        let data = &tcb.tx;
        let _ = self.send_ipv4(Some(*local.ip()), *remote.ip(), PROTO_TCP, header + len, now, tx, |out, src| {
            write_header(out, local.port(), remote.port(), seq, ack, flags, window as u16, header);
            if syn {
                out[20..24].copy_from_slice(&[2, 4, (mss >> 8) as u8, mss as u8]);
                out[24..28].copy_from_slice(&if offer_scale { [1, 3, 3, scale] } else { [1, 1, 1, 1] });
            }
            data.read_at(offset, &mut out[header..]);
            finish(out, src, *remote.ip());
        });
        if flags & ACK != 0 {
            tcb.ack_now = false;
            tcb.ack_at = None;
            tcb.unacked_segments = 0;
        }
        if len > 0 {
            tcb.last_sent = now;
        }
    }

    pub(crate) fn tcp_poll(&mut self, now: Instant, tx: &mut Tx) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        for id in 0..self.tcp.tcbs.len() {
            if self.tcp.tcbs[id].is_none() {
                continue;
            }
            let due = self.with_tcb(id, |stack, tcb| {
                if tcb.deadline().is_some_and(|d| d <= now) {
                    stack.tcp_timers(tcb, now, tx);
                }
                tcb.deadline()
            });
            if let Some(due) = due {
                next = Some(next.map_or(due, |n| n.min(due)));
            }
        }
        next
    }

    fn tcp_timers(&mut self, tcb: &mut Tcb, now: Instant, tx: &mut Tx) {
        if tcb.close_at.is_some_and(|t| t <= now) {
            tcb.close_with(None);
            return;
        }
        if tcb.retransmit_at.is_some_and(|t| t <= now) {
            let limit = if tcb.state.synchronized() { MAX_RETRIES } else { MAX_SYN_RETRIES };
            tcb.retries += 1;
            if tcb.retries > limit {
                let timeout = if tcb.state == State::SynSent { Error::TimedOut } else { Error::ConnectionAborted };
                let error = tcb.soft_error.unwrap_or(timeout);
                tcb.released |= tcb.listener.is_some();
                tcb.close_with(Some(error));
                return;
            }
            tcb.rto = (tcb.rto * 2).min(RTO_MAX);
            tcb.ssthresh = (tcb.in_flight() / 2).max(2 * tcb.snd_mss);
            tcb.cwnd = tcb.snd_mss;
            tcb.dupacks = 0;
            tcb.recover = None;
            tcb.rtt_sample = None;
            tcb.snd_nxt = tcb.snd_una;
            tcb.fin_sent = false;
            tcb.retransmit_at = Some(now + tcb.rto);
        }
        if tcb.ack_at.is_some_and(|t| t <= now) {
            tcb.ack_now = true;
        }
        if tcb.persist_at.is_some_and(|t| t <= now) {
            tcb.persist_at = None;
            tcb.probe = tcb.unsent() > 0;
        }
        if tcb.keepalive_at().is_some_and(|t| t <= now) {
            if tcb.keepalive_probes >= KEEPALIVE_PROBES {
                tcb.close_with(Some(tcb.soft_error.unwrap_or(Error::TimedOut)));
                return;
            }
            tcb.keepalive_probes += 1;
            self.send_tcp(tcb, tcb.snd_una.wrapping_sub(1), ACK, 0, 0, now, tx);
        }
        self.tcp_output(tcb, now, tx);
    }
}

fn write_header(out: &mut [u8], src: u16, dst: u16, seq: u32, ack: u32, flags: u8, window: u16, header: usize) {
    put16(out, 0, src);
    put16(out, 2, dst);
    out[4..8].copy_from_slice(&seq.to_be_bytes());
    out[8..12].copy_from_slice(&ack.to_be_bytes());
    out[12] = ((header / 4) as u8) << 4;
    out[13] = flags;
    put16(out, 14, window);
    put16(out, 16, 0);
    put16(out, 18, 0);
}

fn finish(out: &mut [u8], src: Ipv4Addr, dst: Ipv4Addr) {
    let c = checksum(sum(out, pseudo_sum(src, dst, out.len())));
    put16(out, 16, c);
}
