/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::time::Duration;

use idf::fs::File;

use super::conn::{Conn, Limits, Pool};
use super::h1::{self, Framing, Head};
#[cfg(CONFIG_DEVKIT_NET_HTTP2)]
use super::h2::H2;
use super::{Headers, Version};
use crate::tcp_socket::{blocking, blocking_timeout};
use crate::{Error, Result};

const MAX_LINE: usize = 8 * 1024;
const DISCARD_BYTES: u64 = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chunk {
    Size,
    Data(u64),
    DataEnd,
    Trailer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Length(u64),
    Chunked(Chunk),
    Close,
    Done,
}

/// The body as the connection carries it, without the chunk framing.
struct Raw {
    conn: Option<Conn>,
    pool: Arc<Pool>,
    state: State,
    keep_alive: bool,
    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    stream: Option<(Arc<H2>, u32)>,
    received: u64,
}

impl Drop for Raw {
    fn drop(&mut self) {
        self.close();
    }
}

impl Raw {
    fn ended(&self) -> bool {
        self.state == State::Done
    }

    fn close(&mut self) {
        self.conn = None;
        #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
        if let Some((h2, id)) = self.stream.take() {
            h2.cancel(id);
        }
    }

    fn finish(&mut self) {
        self.state = State::Done;
        if let Some(conn) = self.conn.take() {
            if self.keep_alive && conn.buffered().is_empty() {
                self.pool.put(conn);
            }
        }
    }

    async fn read(&mut self, out: &mut [u8], limits: &Limits) -> Result<usize> {
        #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
        if let Some((h2, id)) = &self.stream {
            let n = h2.read(*id, out, limits).await?;
            if n == 0 {
                self.stream = None;
                self.state = State::Done;
            }
            self.received += n as u64;
            return Ok(n);
        }
        loop {
            match self.state {
                State::Done => return Ok(0),
                State::Length(0) => {
                    self.finish();
                    return Ok(0);
                }
                _ => {}
            }
            let Some(conn) = self.conn.as_mut() else { return Err(Error::NotConnected) };
            match self.state {
                State::Length(left) => {
                    let want = out.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                    let n = conn.read(&mut out[..want], limits).await?;
                    if n == 0 {
                        return Err(Error::ConnectionAborted);
                    }
                    self.received += n as u64;
                    self.state = State::Length(left - n as u64);
                    if left == n as u64 {
                        self.finish();
                    }
                    return Ok(n);
                }
                State::Close => {
                    let n = conn.read(out, limits).await?;
                    if n == 0 {
                        self.finish();
                    }
                    self.received += n as u64;
                    return Ok(n);
                }
                State::Chunked(Chunk::Size) => {
                    let line = conn.line(limits, MAX_LINE).await?;
                    let size = line.split(';').next().unwrap_or("").trim();
                    if size.is_empty() || size.len() > 16 {
                        return Err(Error::InvalidData);
                    }
                    let size = u64::from_str_radix(size, 16).map_err(|_| Error::InvalidData)?;
                    self.state = State::Chunked(if size == 0 { Chunk::Trailer } else { Chunk::Data(size) });
                }
                State::Chunked(Chunk::Data(left)) => {
                    let want = out.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                    let n = conn.read(&mut out[..want], limits).await?;
                    if n == 0 {
                        return Err(Error::ConnectionAborted);
                    }
                    self.received += n as u64;
                    let left = left - n as u64;
                    self.state = State::Chunked(if left == 0 { Chunk::DataEnd } else { Chunk::Data(left) });
                    return Ok(n);
                }
                State::Chunked(Chunk::DataEnd) => {
                    if !conn.line(limits, MAX_LINE).await?.is_empty() {
                        return Err(Error::InvalidData);
                    }
                    self.state = State::Chunked(Chunk::Size);
                }
                State::Chunked(Chunk::Trailer) => {
                    if conn.line(limits, MAX_LINE).await?.is_empty() {
                        self.finish();
                        return Ok(0);
                    }
                }
                State::Done => unreachable!(),
            }
        }
    }
}

#[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
mod decode {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use devkit_compression::{self as compression, Inflate, Source, Window};
    use idf::heap::{self, Caps};

    use crate::{Error, Result};

    const INPUT_BYTES: usize = 4096;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Coding {
        Gzip,
        Deflate,
    }

    impl Coding {
        pub fn of(value: &str) -> Option<Self> {
            let value = value.trim();
            if value.eq_ignore_ascii_case("gzip") || value.eq_ignore_ascii_case("x-gzip") {
                Some(Coding::Gzip)
            } else if value.eq_ignore_ascii_case("deflate") {
                Some(Coding::Deflate)
            } else {
                None
            }
        }
    }

    /// The most bytes a stream of `n` decoded bytes takes from a compressor
    /// that works as it should: stored blocks and the larger wrapper.
    pub(super) fn bound(n: u64) -> u64 {
        n + (n >> 12) + (n >> 14) + (n >> 25) + 7 + 18
    }

    struct Input<'a> {
        data: &'a [u8],
        used: usize,
        pending: bool,
    }

    impl Source for Input<'_> {
        fn fill_buf(&mut self) -> compression::Result<&[u8]> {
            Ok(&self.data[self.used..])
        }

        fn consume(&mut self, n: usize) {
            self.used += n;
        }

        fn pending(&self) -> bool {
            self.pending
        }
    }

    pub(super) struct Decoder {
        coding: Coding,
        inflate: Box<Inflate, Caps>,
        window: Box<Window, Caps>,
        pub buf: Vec<u8>,
        pub pos: usize,
        pub end: usize,
        started: bool,
    }

    fn corrupt(_: compression::Error) -> Error {
        Error::InvalidData
    }

    impl Decoder {
        pub fn new(coding: Coding) -> Result<Self> {
            let inflate = heap::zeroed_box::<Inflate>(Caps::DEFAULT).map_err(|_| Error::NoMemory)?;
            let window = heap::zeroed_box::<Window>(Caps::DEFAULT).map_err(|_| Error::NoMemory)?;
            let mut buf = Vec::new();
            buf.try_reserve_exact(INPUT_BYTES).map_err(|_| Error::NoMemory)?;
            buf.resize(INPUT_BYTES, 0);
            Ok(Self { coding, inflate, window, buf, pos: 0, end: 0, started: false })
        }

        /// Room after the input held, moved to the front first.
        pub fn space(&mut self) -> &mut [u8] {
            if self.pos > 0 {
                self.buf.copy_within(self.pos..self.end, 0);
                self.end -= self.pos;
                self.pos = 0;
            }
            &mut self.buf[self.end..]
        }

        pub fn ended(&self) -> bool {
            self.inflate.ended()
        }

        /// `None` when it needs more input than it has; `more` is whether
        /// the body has any.
        pub fn decode(&mut self, out: &mut [u8], more: bool) -> Result<Option<usize>> {
            let mut input = Input { data: &self.buf[self.pos..self.end], used: 0, pending: more };
            if !self.started {
                if input.data.len() < 2 && more {
                    return Ok(None);
                }
                match self.coding {
                    Coding::Gzip => self.inflate.start_gzip(&mut input).map_err(corrupt)?,
                    Coding::Deflate if is_zlib(input.data) => self.inflate.start(&mut input).map_err(corrupt)?,
                    Coding::Deflate => self.inflate.start_raw(),
                }
                self.started = true;
            }
            let n = self.inflate.read(&mut input, &mut self.window, out).map_err(corrupt)?;
            self.pos += input.used;
            if n == 0 && !self.inflate.ended() {
                return if more { Ok(None) } else { Err(Error::InvalidData) };
            }
            Ok(Some(n))
        }
    }

    // Servers send "deflate" both with the zlib header the name stands for
    // and without one.
    fn is_zlib(data: &[u8]) -> bool {
        match data {
            [cmf, flg, ..] => cmf & 0x0F == 8 && cmf >> 4 <= 7 && ((u16::from(*cmf) << 8) | u16::from(*flg)) % 31 == 0,
            _ => false,
        }
    }
}

#[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
use decode::Coding;

/// A response: its status and header fields, and the body to read.
pub struct Response {
    pub(crate) status: u16,
    version: Version,
    reason: String,
    headers: Headers,
    url: String,
    content_length: Option<u64>,
    raw: Raw,
    limits: Limits,
    #[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
    coding: Option<Coding>,
    #[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
    decoder: Option<decode::Decoder>,
}

pub(crate) struct Parts {
    pub head: Head,
    pub url: String,
    pub head_request: bool,
    /// The request went out whole, so the connection is in step for another.
    pub complete: bool,
    pub decode: bool,
}

/// What a response's body comes over.
pub(crate) enum Carrier {
    H1(Conn),
    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    H2(Arc<H2>, u32),
}

impl Response {
    pub(crate) fn new(carrier: Carrier, pool: Arc<Pool>, parts: Parts, limits: Limits) -> Result<Self> {
        let Parts { head, url, head_request, complete, decode } = parts;
        let framing = h1::framing(&head, head_request)?;
        let content_length = h1::content_length(&head.headers)?;
        let mut raw = Raw {
            conn: None,
            pool,
            state: State::Close,
            keep_alive: false,
            #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
            stream: None,
            received: 0,
        };
        let version = match carrier {
            Carrier::H1(_) if head.minor == 0 => Version::Http10,
            Carrier::H1(_) => Version::Http11,
            #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
            Carrier::H2(..) => Version::Http2,
        };
        match carrier {
            Carrier::H1(conn) => {
                raw.conn = Some(conn);
                raw.keep_alive = complete && h1::keep_alive(&head) && framing != Framing::Close;
                raw.state = match framing {
                    Framing::Empty => State::Length(0),
                    Framing::Length(n) => State::Length(n),
                    Framing::Chunked => State::Chunked(Chunk::Size),
                    Framing::Close => State::Close,
                };
                if framing == Framing::Empty {
                    raw.finish();
                }
            }
            #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
            Carrier::H2(h2, id) => {
                raw.stream = Some((h2, id));
                if framing == Framing::Empty {
                    raw.close();
                    raw.state = State::Done;
                }
            }
        }
        #[cfg(not(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS))]
        let _ = decode;
        #[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
        let coding = match head.headers.get("content-encoding") {
            Some(value) if decode && framing != Framing::Empty => Coding::of(value),
            _ => None,
        };
        Ok(Self {
            status: head.status,
            version,
            reason: head.reason,
            headers: head.headers,
            url,
            content_length,
            raw,
            limits,
            #[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
            coding,
            #[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
            decoder: None,
        })
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn version(&self) -> Version {
        self.version
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// Where the response came from, after redirects.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The Content-Length field: the body as sent, before it is decoded.
    pub fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    /// Body bytes received so far, counted as `content_length` counts them.
    pub fn received(&self) -> u64 {
        self.raw.received
    }

    fn encoded(&self) -> bool {
        #[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
        return self.coding.is_some();
        #[cfg(not(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS))]
        false
    }

    /// The body, decoded when it was compressed for this client; `Ok(0)` at
    /// its end. Dropping the response before then closes the connection.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        blocking(self.read_async(buf))
    }

    pub fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        blocking_timeout(timeout, self.read_async(buf))
    }

    pub async fn read_async(&mut self, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        #[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
        if let Some(coding) = self.coding {
            return self.read_decoded(coding, buf).await;
        }
        let limits = self.limits;
        self.raw.read(buf, &limits).await
    }

    #[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
    async fn read_decoded(&mut self, coding: Coding, buf: &mut [u8]) -> Result<usize> {
        let limits = self.limits;
        if self.decoder.is_none() {
            self.decoder = Some(decode::Decoder::new(coding)?);
        }
        let decoder = self.decoder.as_mut().unwrap();
        loop {
            if decoder.ended() {
                let mut sink = [0u8; 256];
                while self.raw.read(&mut sink, &limits).await? > 0 {}
                return Ok(0);
            }
            match decoder.decode(buf, !self.raw.ended())? {
                Some(0) => continue,
                Some(n) => return Ok(n),
                None => {}
            }
            let space = decoder.space();
            if space.is_empty() {
                return Err(Error::InvalidData);
            }
            let n = self.raw.read(space, &limits).await?;
            decoder.end += n;
        }
    }

    /// The whole body, at most `limit` bytes of it after decoding; more fails
    /// with `MessageTooLarge`, at once when Content-Length tells it.
    pub fn bytes(&mut self, limit: usize) -> Result<Vec<u8>> {
        blocking(self.bytes_async(limit))
    }

    pub fn bytes_timeout(&mut self, limit: usize, timeout: Duration) -> Result<Vec<u8>> {
        blocking_timeout(timeout, self.bytes_async(limit))
    }

    pub async fn bytes_async(&mut self, limit: usize) -> Result<Vec<u8>> {
        #[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
        let bound = |n: u64| if self.encoded() { decode::bound(n) } else { n };
        #[cfg(not(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS))]
        let bound = |n: u64| n;
        let mut out = Vec::new();
        if let Some(len) = self.content_length.filter(|_| !self.raw.ended()) {
            if len > bound(limit as u64) {
                self.raw.close();
                return Err(Error::MessageTooLarge);
            }
            if !self.encoded() {
                out.try_reserve_exact(len as usize).map_err(|_| Error::NoMemory)?;
            }
        }
        loop {
            let len = out.len();
            if !self.encoded() && self.raw.ended() {
                return Ok(out);
            }
            // The end of a stream that has all its Content-Length is a read
            // away; a probe keeps the exact allocation.
            if len == out.capacity() && !self.encoded() && self.content_length == Some(len as u64) {
                let mut probe = [0u8; 1];
                if self.read_async(&mut probe).await? == 0 {
                    return Ok(out);
                }
                out.try_reserve(1).map_err(|_| Error::NoMemory)?;
                out.push(probe[0]);
                continue;
            }
            if len == out.capacity() {
                out.try_reserve_exact(len.max(4096).min(limit + 1 - len)).map_err(|_| Error::NoMemory)?;
            }
            out.resize(out.capacity().min(limit + 1), 0);
            let n = self.read_async(&mut out[len..]).await;
            out.truncate(len + n.as_ref().copied().unwrap_or(0));
            if n? == 0 {
                return Ok(out);
            }
            if out.len() > limit {
                self.raw.close();
                return Err(Error::MessageTooLarge);
            }
        }
    }

    /// Writes the body to `path`, replacing what was there, and returns its
    /// length. A file cut short by a failure stays.
    pub fn save(&mut self, path: &CStr) -> Result<u64> {
        blocking(self.save_async(path))
    }

    pub fn save_timeout(&mut self, path: &CStr, timeout: Duration) -> Result<u64> {
        blocking_timeout(timeout, self.save_async(path))
    }

    pub async fn save_async(&mut self, path: &CStr) -> Result<u64> {
        let file = File::create(path).map_err(Error::Fs)?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(4096).map_err(|_| Error::NoMemory)?;
        buf.resize(4096, 0);
        let mut total = 0;
        loop {
            let n = self.read_async(&mut buf).await?;
            if n == 0 {
                return Ok(total);
            }
            file.write_all(&buf[..n]).map_err(Error::Fs)?;
            total += n as u64;
        }
    }

    /// Reads a short rest of the body so the connection can serve the next
    /// request; a longer one closes it.
    pub(crate) async fn discard(mut self) {
        if !self.raw.keep_alive || self.content_length.is_some_and(|n| n > DISCARD_BYTES) {
            return;
        }
        let limits = self.limits;
        let mut sink = [0u8; 512];
        while self.raw.received <= DISCARD_BYTES {
            match self.raw.read(&mut sink, &limits).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }
}
