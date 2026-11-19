/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::future::Future;
use core::time::Duration;

use idf::executor;
use idf::sync::Mutex;
use idf::time::Instant;

use super::url::Origin;
use crate::{Error, Result, TcpStream, TlsStream};

const READ_CHUNK: usize = 4096;

pub(crate) enum Transport {
    Tcp(TcpStream),
    Tls(Box<TlsStream>),
    #[cfg(test)]
    Mock(Box<crate::tests::http::Mock>),
}

impl Transport {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            Transport::Tcp(s) => s.read_async(buf).await,
            Transport::Tls(s) => s.read_async(buf).await,
            #[cfg(test)]
            Transport::Mock(m) => m.read(buf).await,
        }
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        match self {
            Transport::Tcp(s) => s.write_all_async(buf).await,
            Transport::Tls(s) => s.write_all_async(buf).await,
            #[cfg(test)]
            Transport::Mock(m) => m.write_all(buf),
        }
    }
}

/// How long one wait on the connection may take: `idle` for each, and none
/// past `deadline`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub idle: Option<Duration>,
    pub deadline: Option<Instant>,
}

impl Limits {
    pub async fn run<T>(&self, wait: Option<Duration>, future: impl Future<Output = Result<T>>) -> Result<T> {
        let mut limit = wait;
        if let Some(deadline) = self.deadline {
            let left = deadline.checked_duration_since(Instant::now()).unwrap_or(Duration::ZERO);
            if left.is_zero() {
                return Err(Error::TimedOut);
            }
            limit = Some(limit.map_or(left, |l| l.min(left)));
        }
        match limit {
            Some(limit) => executor::timeout(limit, future).await.map_err(|_| Error::TimedOut)?,
            None => future.await,
        }
    }
}

/// A connection to one origin, with what it received and nobody read yet.
pub(crate) struct Conn {
    io: Transport,
    pub origin: Origin,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
}

impl Conn {
    pub fn new(io: Transport, origin: Origin) -> Self {
        Self { io, origin, buf: Vec::new(), pos: 0, end: 0 }
    }

    pub fn buffered(&self) -> &[u8] {
        &self.buf[self.pos..self.end]
    }

    pub fn consume(&mut self, n: usize) {
        self.pos += n;
        if self.pos == self.end {
            self.pos = 0;
            self.end = 0;
        }
    }

    /// Receives more after what is buffered; `Ok(0)` once the peer closed.
    pub async fn fill(&mut self, limits: &Limits) -> Result<usize> {
        if self.end == self.buf.len() && self.pos > 0 {
            self.buf.copy_within(self.pos..self.end, 0);
            self.end -= self.pos;
            self.pos = 0;
        }
        if self.end == self.buf.len() {
            let grow = self.buf.len().max(READ_CHUNK);
            self.buf.try_reserve_exact(grow).map_err(|_| Error::NoMemory)?;
            self.buf.resize(self.buf.len() + grow, 0);
        }
        let Self { io, buf, end, .. } = self;
        let n = limits.run(limits.idle, io.read(&mut buf[*end..])).await?;
        *end += n;
        Ok(n)
    }

    /// What is buffered first, then straight from the connection.
    pub async fn read(&mut self, out: &mut [u8], limits: &Limits) -> Result<usize> {
        if self.pos < self.end {
            let n = out.len().min(self.end - self.pos);
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.consume(n);
            return Ok(n);
        }
        limits.run(limits.idle, self.io.read(out)).await
    }

    /// A line without its CRLF or LF.
    pub async fn line(&mut self, limits: &Limits, max: usize) -> Result<String> {
        loop {
            if let Some(i) = self.buffered().iter().position(|&b| b == b'\n') {
                let line = &self.buffered()[..i];
                let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned();
                self.consume(i + 1);
                return Ok(line);
            }
            if self.buffered().len() > max {
                return Err(Error::InvalidData);
            }
            if self.fill(limits).await? == 0 {
                return Err(Error::ConnectionAborted);
            }
        }
    }

    pub async fn write_all(&mut self, data: &[u8], limits: &Limits) -> Result<()> {
        limits.run(limits.idle, self.io.write_all(data)).await
    }
}

/// Connections whose last response was read to its end, to send the next
/// request on.
pub(crate) struct Pool {
    idle: Mutex<Vec<Conn>>,
    max: usize,
}

impl Pool {
    pub fn new(max: usize) -> Self {
        Self { idle: Mutex::new(Vec::new()), max }
    }

    pub fn take(&self, origin: &Origin) -> Option<Conn> {
        let mut idle = self.idle.lock();
        let i = idle.iter().rposition(|c| c.origin == *origin)?;
        Some(idle.remove(i))
    }

    /// Drops the oldest when full.
    pub fn put(&self, conn: Conn) {
        if self.max == 0 {
            return;
        }
        let old = {
            let mut idle = self.idle.lock();
            let old = if idle.len() >= self.max { Some(idle.remove(0)) } else { None };
            idle.push(conn);
            old
        };
        drop(old);
    }
}
