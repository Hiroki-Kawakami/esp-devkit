/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::future::Future;
use core::net::{SocketAddr, SocketAddrV4};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use idf::executor;
use idf::heap::Caps;

use crate::addr::{self, ToSocketAddrs};
use crate::net::{self, Net, wait_for};
use crate::stack::SocketId;
use crate::tcp::Buffers;
use crate::{Error, Result};

const DEFAULT_BACKLOG: usize = 4;

fn v4(addr: SocketAddr) -> Result<SocketAddrV4> {
    match addr {
        SocketAddr::V4(addr) => Ok(addr),
        SocketAddr::V6(_) => Err(Error::Unsupported),
    }
}

pub(crate) fn blocking<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    executor::block_on(future)
}

pub(crate) fn blocking_timeout<T>(timeout: Duration, future: impl Future<Output = Result<T>>) -> Result<T> {
    executor::block_on(executor::timeout(timeout, future)).map_err(|_| Error::TimedOut)?
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shutdown {
    Read,
    Write,
    Both,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct TcpBuilder {
    rx_buffer: Option<usize>,
    tx_buffer: Option<usize>,
    caps: Option<Caps>,
    backlog: Option<usize>,
}

impl TcpBuilder {
    /// Also the largest window advertised to the peer.
    pub fn rx_buffer(mut self, bytes: usize) -> Self {
        self.rx_buffer = Some(bytes);
        self
    }

    pub fn tx_buffer(mut self, bytes: usize) -> Self {
        self.tx_buffer = Some(bytes);
        self
    }

    pub fn caps(mut self, caps: Caps) -> Self {
        self.caps = Some(caps);
        self
    }

    /// Connections a listener holds, handshaking or waiting for `accept`;
    /// further ones are ignored until it takes one.
    pub fn backlog(mut self, connections: usize) -> Self {
        self.backlog = Some(connections);
        self
    }

    fn buffers(&self, net: &Net) -> Buffers {
        Buffers {
            rx: self.rx_buffer.unwrap_or(net.config().tcp_rx_buffer),
            tx: self.tx_buffer.unwrap_or(net.config().tcp_tx_buffer),
            caps: self.caps.unwrap_or(net.config().caps),
        }
    }

    /// Accepted connections get this builder's buffers.
    pub fn listen(self, addr: SocketAddr) -> Result<TcpListener> {
        self.listen_in(net::global()?, addr)
    }

    pub(crate) fn listen_in(self, net: &'static Net, addr: SocketAddr) -> Result<TcpListener> {
        let addr = v4(addr)?;
        let buffers = self.buffers(net);
        let id = net.lock().stack.tcp_listen(addr, self.backlog.unwrap_or(DEFAULT_BACKLOG), buffers)?;
        Ok(TcpListener { net, id })
    }

    /// Tries each address in turn and fails with the last one's error.
    pub fn connect(self, addr: impl ToSocketAddrs) -> Result<TcpStream> {
        blocking(net::boxed(|| self.connect_async(addr)))
    }

    /// `timeout` covers the lookup too.
    pub fn connect_timeout(self, addr: impl ToSocketAddrs, timeout: Duration) -> Result<TcpStream> {
        blocking_timeout(timeout, net::boxed(|| self.connect_async(addr)))
    }

    pub async fn connect_async(self, addr: impl ToSocketAddrs) -> Result<TcpStream> {
        self.connect_in(net::global()?, addr).await
    }

    pub(crate) async fn connect_in(self, net: &'static Net, addr: impl ToSocketAddrs) -> Result<TcpStream> {
        let mut last = Error::InvalidInput;
        for addr in addr::resolve(net, addr.target()?).await? {
            match self.connect_to(net, addr).await {
                Ok(stream) => return Ok(stream),
                Err(err) => last = err,
            }
        }
        Err(last)
    }

    pub(crate) async fn connect_to(self, net: &'static Net, addr: SocketAddr) -> Result<TcpStream> {
        let addr = v4(addr)?;
        let buffers = self.buffers(net);
        let now = net::now();
        let id = net.lock_async().await.with_tx(|stack, tx| stack.tcp_connect(addr, buffers, now, tx))?;
        let conn = Arc::new(Conn { net, id, read_closed: AtomicBool::new(false) });
        wait_for(net, SocketId::Tcp(id), |inner, _| inner.stack.tcp_connected(id)).await?;
        Ok(TcpStream { conn })
    }
}

/// Follows `std::net::TcpListener`.
pub struct TcpListener {
    net: &'static Net,
    id: usize,
}

impl TcpListener {
    /// Port 0 picks a free port.
    pub fn bind(addr: SocketAddr) -> Result<Self> {
        TcpBuilder::default().listen(addr)
    }

    pub fn builder() -> TcpBuilder {
        TcpBuilder::default()
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.net.lock().stack.tcp_listener_addr(self.id).into())
    }

    pub fn accept(&self) -> Result<(TcpStream, SocketAddr)> {
        blocking(self.accept_async())
    }

    pub fn accept_timeout(&self, timeout: Duration) -> Result<(TcpStream, SocketAddr)> {
        blocking_timeout(timeout, self.accept_async())
    }

    pub async fn accept_async(&self) -> Result<(TcpStream, SocketAddr)> {
        let id = wait_for(self.net, SocketId::Listener(self.id), |inner, _| inner.stack.tcp_accept(self.id)).await;
        let conn = Arc::new(Conn { net: self.net, id, read_closed: AtomicBool::new(false) });
        let (_, peer) = self.net.lock_async().await.stack.tcp_addrs(id);
        Ok((TcpStream { conn }, peer.into()))
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        let now = net::now();
        let mut inner = self.net.lock();
        inner.forget(SocketId::Listener(self.id));
        inner.with_tx(|stack, tx| stack.tcp_unlisten(self.id, now, tx));
    }
}

struct Conn {
    net: &'static Net,
    id: usize,
    read_closed: AtomicBool,
}

impl Conn {
    fn addrs(&self) -> (SocketAddr, SocketAddr) {
        let (local, remote) = self.net.lock().stack.tcp_addrs(self.id);
        (local.into(), remote.into())
    }

    async fn read(&self, buf: &mut [u8]) -> Result<usize> {
        if self.read_closed.load(Ordering::Relaxed) || buf.is_empty() {
            return Ok(0);
        }
        wait_for(self.net, SocketId::Tcp(self.id), |inner, now| {
            inner.with_tx(|stack, tx| stack.tcp_read(self.id, buf, now, tx))
        })
        .await
    }

    async fn write(&self, buf: &[u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        wait_for(self.net, SocketId::Tcp(self.id), |inner, now| {
            inner.with_tx(|stack, tx| stack.tcp_write(self.id, buf, now, tx))
        })
        .await
    }

    async fn write_all(&self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            let n = self.write(buf).await?;
            buf = &buf[n..];
        }
        Ok(())
    }

    fn shutdown_write(&self) {
        let now = net::now();
        self.net.lock().with_tx(|stack, tx| stack.tcp_shutdown(self.id, now, tx));
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let now = net::now();
        let mut inner = self.net.lock();
        inner.forget(SocketId::Tcp(self.id));
        inner.with_tx(|stack, tx| stack.tcp_release(self.id, now, tx));
    }
}

/// Follows `std::net::TcpStream`. Dropping it closes the connection in the
/// background; with received data left unread, it resets it instead.
pub struct TcpStream {
    conn: Arc<Conn>,
}

impl TcpStream {
    pub fn connect(addr: impl ToSocketAddrs) -> Result<Self> {
        TcpBuilder::default().connect(addr)
    }

    pub fn connect_timeout(addr: impl ToSocketAddrs, timeout: Duration) -> Result<Self> {
        TcpBuilder::default().connect_timeout(addr, timeout)
    }

    pub async fn connect_async(addr: impl ToSocketAddrs) -> Result<Self> {
        TcpBuilder::default().connect_async(addr).await
    }

    pub fn builder() -> TcpBuilder {
        TcpBuilder::default()
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.conn.addrs().0)
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        Ok(self.conn.addrs().1)
    }

    pub fn set_nodelay(&self, nodelay: bool) -> Result<()> {
        self.conn.net.lock().stack.tcp_set_nodelay(self.conn.id, nodelay);
        Ok(())
    }

    pub fn nodelay(&self) -> Result<bool> {
        Ok(self.conn.net.lock().stack.tcp_nodelay(self.conn.id))
    }

    /// Probes the peer after `idle` without traffic, every 75 s, and gives the
    /// connection up with `TimedOut` after 9 unanswered probes. Off by default.
    pub fn set_keepalive(&self, idle: Option<Duration>) -> Result<()> {
        self.conn.net.lock().stack.tcp_set_keepalive(self.conn.id, idle);
        self.conn.net.kick();
        Ok(())
    }

    pub fn keepalive(&self) -> Result<Option<Duration>> {
        Ok(self.conn.net.lock().stack.tcp_keepalive(self.conn.id))
    }

    /// `Ok(0)` once the peer has closed its side.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        blocking(self.conn.read(buf))
    }

    pub fn read_timeout(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        blocking_timeout(timeout, self.conn.read(buf))
    }

    pub async fn read_async(&self, buf: &mut [u8]) -> Result<usize> {
        self.conn.read(buf).await
    }

    /// Queues what fits in the send buffer, waiting only while none does.
    pub fn write(&self, buf: &[u8]) -> Result<usize> {
        blocking(self.conn.write(buf))
    }

    pub fn write_timeout(&self, buf: &[u8], timeout: Duration) -> Result<usize> {
        blocking_timeout(timeout, self.conn.write(buf))
    }

    pub async fn write_async(&self, buf: &[u8]) -> Result<usize> {
        self.conn.write(buf).await
    }

    pub fn write_all(&self, buf: &[u8]) -> Result<()> {
        blocking(self.conn.write_all(buf))
    }

    pub async fn write_all_async(&self, buf: &[u8]) -> Result<()> {
        self.conn.write_all(buf).await
    }

    /// `Read` only makes later reads return `Ok(0)`; the peer is not told.
    pub fn shutdown(&self, how: Shutdown) -> Result<()> {
        if how != Shutdown::Write {
            self.conn.read_closed.store(true, Ordering::Relaxed);
        }
        if how != Shutdown::Read {
            self.conn.shutdown_write();
        }
        Ok(())
    }

    /// The halves may be used from different tasks. Dropping the writer
    /// sends FIN; the connection closes when both are gone.
    pub fn split(self) -> (TcpReader, TcpWriter) {
        (TcpReader { conn: self.conn.clone() }, TcpWriter { conn: self.conn })
    }
}

pub struct TcpReader {
    conn: Arc<Conn>,
}

impl TcpReader {
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.conn.addrs().0)
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        Ok(self.conn.addrs().1)
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        blocking(self.conn.read(buf))
    }

    pub fn read_timeout(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        blocking_timeout(timeout, self.conn.read(buf))
    }

    pub async fn read_async(&self, buf: &mut [u8]) -> Result<usize> {
        self.conn.read(buf).await
    }
}

pub struct TcpWriter {
    conn: Arc<Conn>,
}

impl TcpWriter {
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.conn.addrs().0)
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        Ok(self.conn.addrs().1)
    }

    pub fn write(&self, buf: &[u8]) -> Result<usize> {
        blocking(self.conn.write(buf))
    }

    pub fn write_timeout(&self, buf: &[u8], timeout: Duration) -> Result<usize> {
        blocking_timeout(timeout, self.conn.write(buf))
    }

    pub async fn write_async(&self, buf: &[u8]) -> Result<usize> {
        self.conn.write(buf).await
    }

    pub fn write_all(&self, buf: &[u8]) -> Result<()> {
        blocking(self.conn.write_all(buf))
    }

    pub async fn write_all_async(&self, buf: &[u8]) -> Result<()> {
        self.conn.write_all(buf).await
    }

    pub fn shutdown(&self) -> Result<()> {
        self.conn.shutdown_write();
        Ok(())
    }
}

impl Drop for TcpWriter {
    fn drop(&mut self) {
        self.conn.shutdown_write();
    }
}
