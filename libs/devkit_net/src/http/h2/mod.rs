/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! HTTP/2 (RFC 9113) on a TLS connection whose ALPN chose h2. No task of its
//! own: whichever stream waits takes the reader and hands out what it reads.

mod frame;
mod hpack;
#[cfg(test)]
mod tests;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::{Poll, Waker};

use idf::sync::Mutex;

use super::conn::{Limits, Transport};
use super::h1::{Head, MAX_HEAD};
use super::url::Origin;
use super::{Headers, Windows};
use crate::{Error, Result, TlsReader, TlsWriter};

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const MAX_FRAME: usize = 16384;
const MAX_STREAMS: u32 = 8;
const TABLE_SIZE: usize = 4096;
const DEFAULT_WINDOW: i64 = 65535;
const MAX_WINDOW: i64 = 0x7FFF_FFFF;

enum ReadHalf {
    Tls(TlsReader),
    #[cfg(test)]
    Mock(crate::tests::http::Mock),
}

enum WriteHalf {
    Tls(TlsWriter),
    #[cfg(test)]
    Mock(crate::tests::http::Mock),
}

impl ReadHalf {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            ReadHalf::Tls(r) => r.read_async(buf).await,
            #[cfg(test)]
            ReadHalf::Mock(m) => m.read(buf).await,
        }
    }
}

impl WriteHalf {
    async fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        match self {
            WriteHalf::Tls(w) => w.write_all_async(buf).await,
            #[cfg(test)]
            WriteHalf::Mock(m) => m.write_all(buf),
        }
    }
}

pub(crate) struct Halves(ReadHalf, WriteHalf);

/// The connection split for HTTP/2 when its ALPN chose h2; back as it was
/// otherwise.
pub(crate) fn split(io: Transport) -> core::result::Result<Halves, Transport> {
    match io {
        Transport::Tls(tls) if tls.connection().alpn() == Some(&b"h2"[..]) => {
            let (r, w) = (*tls).split();
            Ok(Halves(ReadHalf::Tls(r), WriteHalf::Tls(w)))
        }
        #[cfg(test)]
        Transport::Mock(m) if m.h2() => Ok(Halves(ReadHalf::Mock((*m).clone()), WriteHalf::Mock(*m))),
        io => Err(io),
    }
}

/// A header block for a request: the pseudo-header fields, then `fields`
/// without the ones HTTP/2 leaves out.
pub(crate) fn request_block(method: &str, tls: bool, authority: &str, path: &str, fields: &[(String, String)]) -> Vec<u8> {
    let mut block = Vec::new();
    hpack::encode(&mut block, ":method", method);
    hpack::encode(&mut block, ":scheme", if tls { "https" } else { "http" });
    hpack::encode(&mut block, ":authority", authority);
    hpack::encode(&mut block, ":path", path);
    for (name, value) in fields {
        let name = name.to_ascii_lowercase();
        let te = name == "te" && !value.eq_ignore_ascii_case("trailers");
        if te || matches!(name.as_str(), "host" | "connection" | "keep-alive" | "proxy-connection" | "transfer-encoding" | "upgrade") {
            continue;
        }
        hpack::encode(&mut block, &name, value);
    }
    block
}

struct Block {
    stream: u32,
    data: Vec<u8>,
    end_stream: bool,
}

struct Reader {
    io: ReadHalf,
    buf: Vec<u8>,
    end: usize,
    hpack: hpack::Decoder,
    /// A header block waiting for its CONTINUATION frames.
    block: Option<Block>,
}

#[derive(Default)]
struct Stream {
    id: u32,
    head: Option<Head>,
    continued: bool,
    data: VecDeque<u8>,
    ended: bool,
    reset: bool,
    /// The request has not ended with END_STREAM yet.
    local_open: bool,
    send_window: i64,
    recv_window: i64,
    unacked: u32,
}

struct State {
    streams: Vec<Stream>,
    opening: u32,
    next_id: u32,
    max_streams: u32,
    peer_window: i64,
    max_send_frame: usize,
    send_window: i64,
    recv_window: i64,
    unacked: u32,
    windows: Windows,
    /// Frames for the next write to send first.
    control: Vec<u8>,
    goaway: bool,
    dead: Option<Error>,
    wakers: Vec<Waker>,
}

impl State {
    fn stream(&mut self, id: u32) -> Option<&mut Stream> {
        self.streams.iter_mut().find(|s| s.id == id)
    }

    fn wake(&mut self) {
        for waker in self.wakers.drain(..) {
            waker.wake();
        }
    }

    /// `n` bytes of `id` read or dropped: room the server gets back, half a
    /// window at a time.
    fn consumed(&mut self, id: Option<u32>, n: usize) {
        self.unacked += n as u32;
        if self.unacked >= self.windows.connection / 2 {
            frame::window_update(&mut self.control, 0, self.unacked);
            self.recv_window += i64::from(self.unacked);
            self.unacked = 0;
        }
        let half = self.windows.stream / 2;
        let Some(s) = id.and_then(|id| self.streams.iter_mut().find(|s| s.id == id)) else { return };
        s.unacked += n as u32;
        if s.unacked >= half && !s.ended {
            frame::window_update(&mut self.control, s.id, s.unacked);
            s.recv_window += i64::from(s.unacked);
            s.unacked = 0;
        }
    }

    fn remove(&mut self, id: u32) {
        let Some(i) = self.streams.iter().position(|s| s.id == id) else { return };
        let s = self.streams.remove(i);
        if (s.local_open || !s.ended) && !s.reset {
            frame::rst_stream(&mut self.control, id, frame::CANCEL);
        }
        self.consumed(None, s.data.len());
        self.wake();
    }
}

pub(crate) struct H2 {
    pub origin: Origin,
    reader: Mutex<Reader>,
    writer: Mutex<WriteHalf>,
    state: Mutex<State>,
}

/// A stream, given up when this is dropped before `release`.
pub(crate) struct StreamRef {
    h2: Option<Arc<H2>>,
    pub id: u32,
}

impl StreamRef {
    pub fn new(h2: Arc<H2>, id: u32) -> Self {
        Self { h2: Some(h2), id }
    }

    pub fn h2(&self) -> &H2 {
        self.h2.as_ref().unwrap()
    }

    pub fn release(mut self) -> (Arc<H2>, u32) {
        (self.h2.take().unwrap(), self.id)
    }
}

impl Drop for StreamRef {
    fn drop(&mut self) {
        if let Some(h2) = &self.h2 {
            h2.cancel(self.id);
        }
    }
}

/// Ends the connection when a write stops partway, as a cancelled one does.
struct Writing<'a>(Option<&'a H2>);

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        if let Some(h2) = self.0 {
            h2.fail(Error::ConnectionAborted);
        }
    }
}

/// A slot `open` holds until the stream it is for is in the table.
struct Opening<'a>(&'a H2);

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        let mut st = self.0.state.lock();
        st.opening -= 1;
        st.wake();
    }
}

impl H2 {
    pub async fn start(halves: Halves, origin: Origin, windows: Windows, limits: &Limits) -> Result<Arc<Self>> {
        let Halves(read, write) = halves;
        let mut buf = Vec::new();
        buf.try_reserve_exact(frame::HEADER + MAX_FRAME).map_err(|_| Error::NoMemory)?;
        buf.resize(frame::HEADER + MAX_FRAME, 0);
        let mut first = PREFACE.to_vec();
        frame::settings(
            &mut first,
            &[
                (frame::SETTINGS_ENABLE_PUSH, 0),
                (frame::SETTINGS_INITIAL_WINDOW_SIZE, windows.stream),
                (frame::SETTINGS_MAX_HEADER_LIST_SIZE, MAX_HEAD as u32),
            ],
        );
        let extra = i64::from(windows.connection) - DEFAULT_WINDOW;
        if extra > 0 {
            frame::window_update(&mut first, 0, extra as u32);
        }
        let h2 = Arc::new(Self {
            origin,
            reader: Mutex::new(Reader { io: read, buf, end: 0, hpack: hpack::Decoder::new(TABLE_SIZE), block: None }),
            writer: Mutex::new(write),
            state: Mutex::new(State {
                streams: Vec::new(),
                opening: 0,
                next_id: 1,
                max_streams: MAX_STREAMS,
                peer_window: DEFAULT_WINDOW,
                max_send_frame: MAX_FRAME,
                send_window: DEFAULT_WINDOW,
                recv_window: DEFAULT_WINDOW.max(i64::from(windows.connection)),
                unacked: 0,
                windows,
                control: Vec::new(),
                goaway: false,
                dead: None,
                wakers: Vec::new(),
            }),
        });
        limits.run(limits.idle, h2.write(&first)).await?;
        Ok(h2)
    }

    /// Takes new streams: alive and not told to go away.
    pub fn usable(&self) -> bool {
        let st = self.state.lock();
        st.dead.is_none() && !st.goaway
    }

    fn fail(&self, e: Error) {
        let mut st = self.state.lock();
        st.dead.get_or_insert(e);
        st.wake();
    }

    async fn write(&self, data: &[u8]) -> Result<()> {
        let mut w = self.writer.lock_async().await;
        self.write_locked(&mut w, data).await
    }

    async fn write_locked(&self, w: &mut WriteHalf, data: &[u8]) -> Result<()> {
        let control = core::mem::take(&mut self.state.lock().control);
        let mut writing = Writing(Some(self));
        let result = match w.write_all(&control).await {
            Ok(()) => w.write_all(data).await,
            Err(e) => Err(e),
        };
        writing.0 = None;
        if let Err(e) = result {
            self.fail(e);
        }
        result
    }

    async fn flush_control(&self) -> Result<()> {
        if self.state.lock().control.is_empty() {
            return Ok(());
        }
        self.write(&[]).await
    }

    /// Until `ready` has an answer, reading frames meanwhile when no other
    /// stream does.
    async fn wait<T>(&self, mut ready: impl FnMut(&mut State) -> Option<Result<T>>) -> Result<T> {
        loop {
            let mut lock = pin!(self.reader.lock_async());
            let reader = poll_fn(|cx| {
                let mut st = self.state.lock();
                if let Some(answer) = ready(&mut st) {
                    return Poll::Ready(Err(answer));
                }
                if let Some(e) = st.dead {
                    return Poll::Ready(Err(Err(e)));
                }
                st.wakers.push(cx.waker().clone());
                drop(st);
                lock.as_mut().poll(cx).map(Ok)
            })
            .await;
            let mut reader = match reader {
                Ok(reader) => reader,
                Err(answer) => return answer,
            };
            if let Some(answer) = ready(&mut self.state.lock()) {
                return answer;
            }
            if self.state.lock().dead.is_some() {
                continue;
            }
            match self.read_frame(&mut reader).await {
                Ok(()) => {}
                Err(Error::InvalidData) => {
                    frame::goaway(&mut self.state.lock().control, 0, frame::PROTOCOL_ERROR);
                    let _ = self.flush_control().await;
                    self.fail(Error::InvalidData);
                }
                Err(e) => self.fail(e),
            }
            drop(reader);
            self.flush_control().await?;
        }
    }

    async fn read_frame(&self, r: &mut Reader) -> Result<()> {
        let len = loop {
            if r.end >= frame::HEADER {
                let (len, ..) = frame::parse(&r.buf);
                if len > MAX_FRAME {
                    return Err(Error::InvalidData);
                }
                if r.end >= frame::HEADER + len {
                    break len;
                }
            }
            let Reader { io, buf, end, .. } = r;
            let n = io.read(&mut buf[*end..]).await?;
            if n == 0 {
                return Err(Error::ConnectionAborted);
            }
            *end += n;
        };
        let (_, kind, flags, stream) = frame::parse(&r.buf);
        let size = frame::HEADER + len;
        let Reader { buf, hpack, block, end, .. } = r;
        let result = self.process(kind, flags, stream, &buf[frame::HEADER..size], hpack, block);
        buf.copy_within(size..*end, 0);
        *end -= size;
        self.state.lock().wake();
        result
    }

    fn process(&self, kind: u8, flags: u8, id: u32, p: &[u8], hpack: &mut hpack::Decoder, block: &mut Option<Block>) -> Result<()> {
        if block.as_ref().is_some_and(|b| kind != frame::CONTINUATION || b.stream != id) {
            return Err(Error::InvalidData);
        }
        match kind {
            frame::DATA => {
                if id == 0 {
                    return Err(Error::InvalidData);
                }
                let (body, pad) = frame::unpad(flags, p).ok_or(Error::InvalidData)?;
                let mut st = self.state.lock();
                st.recv_window -= p.len() as i64;
                if st.recv_window < 0 {
                    return Err(Error::InvalidData);
                }
                let taken = match st.stream(id) {
                    Some(s) if !s.ended && !s.reset => {
                        s.recv_window -= p.len() as i64;
                        if s.recv_window < 0 {
                            return Err(Error::InvalidData);
                        }
                        s.data.extend(body);
                        s.ended = flags & frame::END_STREAM != 0;
                        Some(id)
                    }
                    _ => None,
                };
                let free = if taken.is_some() { pad } else { p.len() };
                if free > 0 {
                    st.consumed(taken, free);
                }
            }
            frame::HEADERS => {
                if id == 0 {
                    return Err(Error::InvalidData);
                }
                let (mut fragment, _) = frame::unpad(flags, p).ok_or(Error::InvalidData)?;
                if flags & frame::PRIORITY_FLAG != 0 {
                    fragment = fragment.get(5..).ok_or(Error::InvalidData)?;
                }
                let end_stream = flags & frame::END_STREAM != 0;
                if flags & frame::END_HEADERS != 0 {
                    self.headers(id, fragment, end_stream, hpack)?;
                } else {
                    *block = Some(Block { stream: id, data: fragment.to_vec(), end_stream });
                }
            }
            frame::CONTINUATION => {
                let b = block.as_mut().ok_or(Error::InvalidData)?;
                if b.data.len() + p.len() > MAX_HEAD {
                    return Err(Error::MessageTooLarge);
                }
                b.data.extend_from_slice(p);
                if flags & frame::END_HEADERS != 0 {
                    let b = block.take().unwrap();
                    self.headers(b.stream, &b.data, b.end_stream, hpack)?;
                }
            }
            frame::RST_STREAM => {
                if p.len() != 4 || id == 0 {
                    return Err(Error::InvalidData);
                }
                let mut st = self.state.lock();
                if let Some(s) = st.stream(id) {
                    s.reset = true;
                }
            }
            frame::SETTINGS => {
                if id != 0 || p.len() % 6 != 0 {
                    return Err(Error::InvalidData);
                }
                if flags & frame::ACK != 0 {
                    return Ok(());
                }
                let mut st = self.state.lock();
                for entry in p.chunks_exact(6) {
                    let value = u32::from_be_bytes([entry[2], entry[3], entry[4], entry[5]]);
                    match u16::from_be_bytes([entry[0], entry[1]]) {
                        frame::SETTINGS_MAX_CONCURRENT_STREAMS => st.max_streams = value.min(MAX_STREAMS),
                        frame::SETTINGS_INITIAL_WINDOW_SIZE => {
                            let value = i64::from(value);
                            if value > MAX_WINDOW {
                                return Err(Error::InvalidData);
                            }
                            let delta = value - st.peer_window;
                            st.peer_window = value;
                            for s in &mut st.streams {
                                s.send_window += delta;
                            }
                        }
                        frame::SETTINGS_MAX_FRAME_SIZE => {
                            if !(16384..=0xFF_FFFF).contains(&value) {
                                return Err(Error::InvalidData);
                            }
                            st.max_send_frame = (value as usize).min(MAX_FRAME);
                        }
                        frame::SETTINGS_HEADER_TABLE_SIZE | frame::SETTINGS_ENABLE_PUSH | frame::SETTINGS_MAX_HEADER_LIST_SIZE => {}
                        _ => {}
                    }
                }
                frame::header(&mut st.control, 0, frame::SETTINGS, frame::ACK, 0);
            }
            frame::PUSH_PROMISE => return Err(Error::InvalidData),
            frame::PRIORITY => {}
            frame::PING => {
                if p.len() != 8 || id != 0 {
                    return Err(Error::InvalidData);
                }
                if flags & frame::ACK == 0 {
                    let mut st = self.state.lock();
                    frame::header(&mut st.control, 8, frame::PING, frame::ACK, 0);
                    st.control.extend_from_slice(p);
                }
            }
            frame::GOAWAY => {
                if p.len() < 8 || id != 0 {
                    return Err(Error::InvalidData);
                }
                let last = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7FFF_FFFF;
                let mut st = self.state.lock();
                st.goaway = true;
                for s in st.streams.iter_mut().filter(|s| s.id > last) {
                    s.reset = true;
                }
            }
            frame::WINDOW_UPDATE => {
                if p.len() != 4 {
                    return Err(Error::InvalidData);
                }
                let increment = i64::from(u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7FFF_FFFF);
                let mut st = self.state.lock();
                if id == 0 {
                    if increment == 0 {
                        return Err(Error::InvalidData);
                    }
                    st.send_window += increment;
                } else if let Some(s) = st.stream(id) {
                    s.send_window += increment;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// A header block is decoded even for a stream that is gone, to keep the
    /// table in step with the server's.
    fn headers(&self, id: u32, block: &[u8], end_stream: bool, hpack: &mut hpack::Decoder) -> Result<()> {
        let fields = hpack.decode(block, MAX_HEAD)?;
        let mut st = self.state.lock();
        let Some(s) = st.stream(id) else { return Ok(()) };
        if s.head.is_some() || s.ended {
            s.ended |= end_stream;
            return Ok(());
        }
        let mut status = None;
        let mut headers = Headers::new();
        for (name, value) in &fields {
            match name.as_str() {
                ":status" => status = value.parse::<u16>().ok(),
                n if n.starts_with(':') => {}
                n => headers.append(n, value),
            }
        }
        let status = status.filter(|s| (100..600).contains(s)).ok_or(Error::InvalidData)?;
        if status < 200 {
            s.continued |= status == 100;
            return if end_stream { Err(Error::InvalidData) } else { Ok(()) };
        }
        s.head = Some(Head { status, reason: String::new(), minor: 0, headers });
        s.ended = end_stream;
        Ok(())
    }

    /// Sends a request's header block on a new stream, once the server allows
    /// another; a connection told to go away fails with `ConnectionReset`.
    pub async fn open(&self, block: &[u8], end_stream: bool, limits: &Limits) -> Result<u32> {
        let slot = self.wait(|st| {
            if st.goaway {
                return Some(Err(Error::ConnectionReset));
            }
            if st.streams.len() as u32 + st.opening < st.max_streams {
                st.opening += 1;
                return Some(Ok(()));
            }
            None
        });
        limits.run(limits.idle, slot).await?;
        let opening = Opening(self);
        limits
            .run(limits.idle, async {
                // Stream IDs have to go out in order, so one is taken under
                // the writer's lock.
                let mut w = self.writer.lock_async().await;
                let mut frames = Vec::new();
                let id = {
                    let mut st = self.state.lock();
                    let id = st.next_id;
                    st.next_id += 2;
                    let send_window = st.peer_window;
                    let recv_window = i64::from(st.windows.stream);
                    st.streams.push(Stream { id, local_open: !end_stream, send_window, recv_window, ..Stream::default() });
                    frame::headers(&mut frames, id, block, end_stream, st.max_send_frame);
                    id
                };
                drop(opening);
                self.write_locked(&mut w, &frames).await?;
                Ok(id)
            })
            .await
    }

    /// The final response head; `ConnectionReset` when the server reset the
    /// stream or went away before it.
    pub async fn head(&self, id: u32, limits: &Limits) -> Result<Head> {
        let wait = self.wait(|st| {
            let Some(s) = st.stream(id) else { return Some(Err(Error::NotConnected)) };
            if let Some(head) = s.head.take() {
                return Some(Ok(head));
            }
            if s.reset {
                return Some(Err(Error::ConnectionReset));
            }
            if s.ended {
                return Some(Err(Error::InvalidData));
            }
            None
        });
        limits.run(limits.idle, wait).await
    }

    /// `None` after 100 Continue, the final head when it came instead.
    pub async fn continued(&self, id: u32, limits: &Limits) -> Result<Option<Head>> {
        let wait = self.wait(|st| {
            let Some(s) = st.stream(id) else { return Some(Err(Error::NotConnected)) };
            if let Some(head) = s.head.take() {
                return Some(Ok(Some(head)));
            }
            if s.continued {
                return Some(Ok(None));
            }
            if s.reset {
                return Some(Err(Error::ConnectionReset));
            }
            None
        });
        limits.run(limits.idle, wait).await
    }

    /// Body bytes of `id`; `Ok(0)` at its end, which also closes the stream.
    pub async fn read(&self, id: u32, out: &mut [u8], limits: &Limits) -> Result<usize> {
        let wait = self.wait(|st| {
                let Some(s) = st.stream(id) else { return Some(Err(Error::NotConnected)) };
                if !s.data.is_empty() {
                    let n = out.len().min(s.data.len());
                    for (dst, src) in out.iter_mut().zip(s.data.drain(..n)) {
                        *dst = src;
                    }
                    st.consumed(Some(id), n);
                    return Some(Ok(n));
                }
                if s.ended {
                    st.remove(id);
                    return Some(Ok(0));
                }
                if s.reset {
                    st.remove(id);
                    return Some(Err(Error::ConnectionReset));
                }
                None
            });
        let n = limits.run(limits.idle, wait).await?;
        limits.run(limits.idle, self.flush_control()).await?;
        Ok(n)
    }

    /// Sends `data` as the server's windows allow, and ends the request when
    /// `end` is set.
    pub async fn send(&self, id: u32, mut data: &[u8], end: bool, limits: &Limits) -> Result<()> {
        loop {
            let wait = self.wait(|st| {
                    let (conn, max) = (st.send_window, st.max_send_frame as i64);
                    let Some(s) = st.stream(id) else { return Some(Err(Error::NotConnected)) };
                    if s.reset {
                        return Some(Err(Error::ConnectionReset));
                    }
                    if data.is_empty() {
                        return Some(Ok(0));
                    }
                    let room = conn.min(s.send_window).min(max);
                    if room <= 0 {
                        return None;
                    }
                    let n = (room as usize).min(data.len());
                    s.send_window -= n as i64;
                    st.send_window -= n as i64;
                    Some(Ok(n))
                });
            let n = limits.run(limits.idle, wait).await?;
            let last = end && n == data.len();
            let mut out = Vec::new();
            out.try_reserve_exact(frame::HEADER + n).map_err(|_| Error::NoMemory)?;
            frame::header(&mut out, n, frame::DATA, if last { frame::END_STREAM } else { 0 }, id);
            out.extend_from_slice(&data[..n]);
            if last {
                if let Some(s) = self.state.lock().stream(id) {
                    s.local_open = false;
                }
            }
            if n > 0 || last {
                limits.run(limits.idle, self.write(&out)).await?;
            }
            data = &data[n..];
            if data.is_empty() {
                return Ok(());
            }
        }
    }

    /// Gives a stream up: the server is told to stop when it is not done.
    pub fn cancel(&self, id: u32) {
        self.state.lock().remove(id);
    }
}
