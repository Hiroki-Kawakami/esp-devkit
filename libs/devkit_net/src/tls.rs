/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::time::Duration;

use devkit_crypto::tls::{ClientConfig, Connection};

use crate::tcp_socket::{blocking, blocking_timeout};
use crate::{Error, Result, Shutdown, TcpStream};

/// A TLS 1.3 client connection over a [`TcpStream`].
pub struct TlsStream {
    tcp: TcpStream,
    conn: Connection,
}

impl TlsStream {
    /// Connects to `host:port` and completes the handshake; `host` is also the
    /// name the server's certificate has to carry.
    pub fn connect(host: &str, port: u16, config: Arc<ClientConfig>) -> Result<Self> {
        blocking(Self::connect_async(host, port, config))
    }

    pub async fn connect_async(host: &str, port: u16, config: Arc<ClientConfig>) -> Result<Self> {
        let tcp = TcpStream::connect_async((host, port)).await?;
        Self::start_async(tcp, Some(host), config).await
    }

    /// Starts TLS on `tcp` where it stands, as STARTTLS does.
    pub fn start(tcp: TcpStream, server_name: Option<&str>, config: Arc<ClientConfig>) -> Result<Self> {
        blocking(Self::start_async(tcp, server_name, config))
    }

    pub async fn start_async(tcp: TcpStream, server_name: Option<&str>, config: Arc<ClientConfig>) -> Result<Self> {
        let conn = Connection::client(config, server_name).map_err(Error::Tls)?;
        let mut stream = Self { tcp, conn };
        while stream.conn.is_handshaking() {
            stream.flush().await?;
            if !stream.fill().await? {
                return Err(Error::ConnectionAborted);
            }
        }
        stream.flush().await?;
        Ok(stream)
    }

    /// The TLS state: ALPN, the server's certificates, exported keying material.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn tcp(&self) -> &TcpStream {
        &self.tcp
    }

    /// `Ok(0)` once the server sent close_notify. A connection that ends
    /// without it fails with `ConnectionAborted`: the data may be cut short.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        blocking(self.read_async(buf))
    }

    pub fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        blocking_timeout(timeout, self.read_async(buf))
    }

    pub async fn read_async(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            let n = self.conn.read(buf).map_err(Error::Tls)?;
            if n > 0 || buf.is_empty() || self.conn.peer_closed() {
                return Ok(n);
            }
            if !self.fill().await? {
                return Err(Error::ConnectionAborted);
            }
        }
    }

    pub fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        blocking(self.write_all_async(buf))
    }

    pub async fn write_all_async(&mut self, buf: &[u8]) -> Result<()> {
        for chunk in buf.chunks(16 * 1024) {
            self.conn.write(chunk).map_err(Error::Tls)?;
            self.flush().await?;
        }
        Ok(())
    }

    /// Sends close_notify, then FIN.
    pub fn shutdown(&mut self) -> Result<()> {
        blocking(self.shutdown_async())
    }

    pub async fn shutdown_async(&mut self) -> Result<()> {
        self.conn.close();
        self.flush().await?;
        self.tcp.shutdown(Shutdown::Write)
    }

    async fn flush(&mut self) -> Result<()> {
        while !self.conn.pending_tx().is_empty() {
            let n = self.tcp.write_async(self.conn.pending_tx()).await?;
            self.conn.consume_tx(n);
        }
        Ok(())
    }

    /// Reads once from TCP and processes it; `false` at the end of the stream.
    async fn fill(&mut self) -> Result<bool> {
        let n = self.tcp.read_async(self.conn.rx_space()).await?;
        let processed = self.conn.commit_rx(n);
        self.flush().await?;
        processed.map_err(Error::Tls)?;
        Ok(n > 0)
    }
}
