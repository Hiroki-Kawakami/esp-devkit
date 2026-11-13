/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::DerefMut;
use core::time::Duration;

use devkit_crypto::tls::{ClientConfig, Connection};
use idf::sync::Mutex;

use crate::tcp_socket::{blocking, blocking_timeout};
use crate::{Error, Result, Shutdown, TcpReader, TcpStream, TcpWriter};

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
        Self::handshake(tcp, conn).await
    }

    /// As `start_async`, receiving records in `rx`, which holds at least
    /// `Connection::RX_BYTES`.
    pub async fn start_in_async(tcp: TcpStream, server_name: Option<&str>, config: Arc<ClientConfig>, rx: Box<dyn DerefMut<Target = [u8]> + Send>) -> Result<Self> {
        let conn = Connection::client_in(config, server_name, rx).map_err(Error::Tls)?;
        Self::handshake(tcp, conn).await
    }

    async fn handshake(tcp: TcpStream, conn: Connection) -> Result<Self> {
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

    /// The halves may be used from different tasks. Dropping the writer
    /// sends FIN without close_notify; the connection closes when both are gone.
    pub fn split(self) -> (TlsReader, TlsWriter) {
        let (reader, writer) = self.tcp.split();
        let shared = Arc::new(Shared { conn: Mutex::new(self.conn), tx: Mutex::new(Tx { tcp: writer, buf: Vec::new() }) });
        (TlsReader { shared: shared.clone(), tcp: reader }, TlsWriter { shared })
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

struct Tx {
    tcp: TcpWriter,
    buf: Vec<u8>,
}

struct Shared {
    conn: Mutex<Connection>,
    tx: Mutex<Tx>,
}

impl Shared {
    /// Bytes leave `pending_tx` only under `tx`, so they go out in order.
    async fn flush(&self) -> Result<()> {
        let mut tx = self.tx.lock_async().await;
        loop {
            {
                let mut conn = self.conn.lock();
                let pending = conn.pending_tx();
                if pending.is_empty() {
                    return Ok(());
                }
                tx.buf.clear();
                tx.buf.try_reserve(pending.len()).map_err(|_| Error::NoMemory)?;
                tx.buf.extend_from_slice(pending);
                let n = pending.len();
                conn.consume_tx(n);
            }
            let Tx { tcp, buf } = &mut *tx;
            tcp.write_all_async(buf).await?;
        }
    }

    fn has_pending_tx(&self) -> bool {
        !self.conn.lock().pending_tx().is_empty()
    }
}

pub struct TlsReader {
    shared: Arc<Shared>,
    tcp: TcpReader,
}

impl TlsReader {
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        blocking(self.read_async(buf))
    }

    pub fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        blocking_timeout(timeout, self.read_async(buf))
    }

    pub async fn read_async(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            {
                let mut conn = self.shared.conn.lock();
                let n = conn.read(buf).map_err(Error::Tls)?;
                if n > 0 || buf.is_empty() || conn.peer_closed() {
                    drop(conn);
                    if self.shared.has_pending_tx() {
                        self.shared.flush().await?;
                    }
                    return Ok(n);
                }
            }
            // The stack is locked first and without blocking: a task waiting
            // for it on this executor may be handed the lock while this one
            // blocks.
            let mut tcp = self.tcp.lock_async().await;
            let step = {
                let mut conn = self.shared.conn.lock();
                let space = conn.rx_space();
                if space.is_empty() {
                    return Err(Error::ConnectionAborted);
                }
                match tcp.try_read(space) {
                    None => {
                        drop(tcp);
                        Step::Wait
                    }
                    Some(Ok(0)) => return Err(Error::ConnectionAborted),
                    Some(Ok(got)) => {
                        drop(tcp);
                        Step::Read(conn.commit_rx(got))
                    }
                    Some(Err(err)) => return Err(err),
                }
            };
            if self.shared.has_pending_tx() {
                self.shared.flush().await?;
            }
            match step {
                Step::Read(processed) => processed.map_err(Error::Tls)?,
                Step::Wait => self.tcp.readable_async().await,
            }
        }
    }
}

enum Step {
    Read(core::result::Result<(), devkit_crypto::Error>),
    Wait,
}

pub struct TlsWriter {
    shared: Arc<Shared>,
}

impl TlsWriter {
    pub fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        blocking(self.write_all_async(buf))
    }

    pub async fn write_all_async(&mut self, buf: &[u8]) -> Result<()> {
        for chunk in buf.chunks(16 * 1024) {
            self.shared.conn.lock().write(chunk).map_err(Error::Tls)?;
            self.shared.flush().await?;
        }
        Ok(())
    }

    /// Sends close_notify, then FIN.
    pub fn shutdown(&mut self) -> Result<()> {
        blocking(self.shutdown_async())
    }

    pub async fn shutdown_async(&mut self) -> Result<()> {
        self.shared.conn.lock().close();
        self.shared.flush().await?;
        self.shared.tx.lock_async().await.tcp.shutdown()
    }
}
