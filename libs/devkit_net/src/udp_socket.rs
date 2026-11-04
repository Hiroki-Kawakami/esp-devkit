/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::net::{SocketAddr, SocketAddrV4};
use core::time::Duration;

use idf::executor;
use idf::heap::Caps;

use crate::net::{self, Net, retry_while_busy, wait_for};
use crate::stack::{IfaceId, SocketId};
use crate::{Error, Result};

fn v4(addr: SocketAddr) -> Result<SocketAddrV4> {
    match addr {
        SocketAddr::V4(addr) => Ok(addr),
        SocketAddr::V6(_) => Err(Error::Unsupported),
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct UdpBuilder {
    rx_buffer: Option<usize>,
    caps: Option<Caps>,
    iface: Option<IfaceId>,
}

impl UdpBuilder {
    /// Bytes of queued datagrams; one that does not fit is dropped.
    pub fn rx_buffer(mut self, bytes: usize) -> Self {
        self.rx_buffer = Some(bytes);
        self
    }

    pub fn caps(mut self, caps: Caps) -> Self {
        self.caps = Some(caps);
        self
    }

    pub(crate) fn interface(mut self, iface: IfaceId) -> Self {
        self.iface = Some(iface);
        self
    }

    pub fn bind(self, addr: SocketAddr) -> Result<UdpSocket> {
        self.bind_in(net::global()?, addr)
    }

    pub(crate) fn bind_in(self, net: &'static Net, addr: SocketAddr) -> Result<UdpSocket> {
        let addr = v4(addr)?;
        let size = self.rx_buffer.unwrap_or(net.config().udp_rx_buffer);
        let caps = self.caps.unwrap_or(net.config().caps);
        let id = net.lock().stack.udp_bind(addr, self.iface, size, caps)?;
        Ok(UdpSocket { net, id })
    }
}

/// Follows `std::net::UdpSocket`. Calls that only change or read the socket
/// return at once; receiving waits, in the calling task or as a future.
pub struct UdpSocket {
    net: &'static Net,
    id: usize,
}

impl UdpSocket {
    /// Port 0 picks a free port.
    pub fn bind(addr: SocketAddr) -> Result<Self> {
        UdpBuilder::default().bind(addr)
    }

    pub fn builder() -> UdpBuilder {
        UdpBuilder::default()
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.net.lock().stack.udp_local(self.id).into())
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        self.net.lock().stack.udp_peer(self.id).map(Into::into).ok_or(Error::NotConnected)
    }

    /// Sets the destination of `send` and receives only from `addr`.
    pub fn connect(&self, addr: SocketAddr) -> Result<()> {
        let addr = v4(addr)?;
        self.net.lock().stack.udp_connect(self.id, addr);
        Ok(())
    }

    /// Hands the datagram to the link before returning. `Ok` does not mean it
    /// left: one waiting for address resolution is dropped if the resolution
    /// fails or another datagram to the same host replaces it. `WouldBlock`
    /// when the link had no room for it.
    pub fn send_to(&self, buf: &[u8], addr: SocketAddr) -> Result<usize> {
        let addr = v4(addr)?;
        let now = net::now();
        self.net.lock().with_tx(|stack, tx| stack.udp_send(self.id, addr, buf, now, tx))?;
        Ok(buf.len())
    }

    pub fn send(&self, buf: &[u8]) -> Result<usize> {
        let peer = self.peer_addr()?;
        self.send_to(buf, peer)
    }

    /// As `send_to`, waiting while the link has no room instead of failing
    /// with `WouldBlock`.
    pub async fn send_to_async(&self, buf: &[u8], addr: SocketAddr) -> Result<usize> {
        retry_while_busy(self.net, || self.send_to(buf, addr)).await
    }

    pub async fn send_async(&self, buf: &[u8]) -> Result<usize> {
        let peer = self.peer_addr()?;
        self.send_to_async(buf, peer).await
    }

    /// The rest of a datagram longer than `buf` is dropped.
    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        executor::block_on(self.recv_from_async(buf))
    }

    pub fn recv_from_timeout(&self, buf: &mut [u8], timeout: Duration) -> Result<(usize, SocketAddr)> {
        executor::block_on(async { executor::timeout(timeout, self.recv_from_async(buf)).await.map_err(|_| Error::TimedOut)? })
    }

    pub async fn recv_from_async(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let (n, from) = wait_for(self.net, SocketId::Udp(self.id), |inner, _| inner.stack.udp_recv(self.id, buf)).await?;
        Ok((n, from.into()))
    }

    pub fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        self.recv_from(buf).map(|(n, _)| n)
    }

    pub fn recv_timeout(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        self.recv_from_timeout(buf, timeout).map(|(n, _)| n)
    }

    pub async fn recv_async(&self, buf: &mut [u8]) -> Result<usize> {
        self.recv_from_async(buf).await.map(|(n, _)| n)
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        let mut inner = self.net.lock();
        inner.forget(SocketId::Udp(self.id));
        inner.stack.udp_close(self.id);
    }
}
