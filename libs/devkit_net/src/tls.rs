/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::MaybeUninit;
use core::ops::DerefMut;
use core::time::Duration;

use devkit_crypto::tls::{ClientConfig, Connection};
use idf::sync::Mutex;

use crate::tcp_socket::{blocking, blocking_timeout};
use crate::{Error, Result, Shutdown, TcpReadLock, TcpReader, TcpStream, TcpWriter};

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
        self.read_uninit_async(crate::ring::uninit(buf)).await
    }

    /// As `read_async`, filling `buf` without it being initialized first.
    pub async fn read_uninit_async(&mut self, buf: &mut [MaybeUninit<u8>]) -> Result<usize> {
        loop {
            {
                let mut conn = self.shared.conn.lock();
                let n = conn.read_uninit(buf).map_err(Error::Tls)?;
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
            let tcp = self.tcp.lock_async().await;
            let mut conn = self.shared.conn.lock();
            #[cfg(all(target_os = "none", CONFIG_SOC_AES_SUPPORT_GCM, CONFIG_SOC_AES_GDMA))]
            let step = match direct(tcp, &mut conn) {
                Ok(step) => step?,
                Err(tcp) => copied(tcp, &mut conn)?,
            };
            #[cfg(not(all(target_os = "none", CONFIG_SOC_AES_SUPPORT_GCM, CONFIG_SOC_AES_GDMA)))]
            let step = copied(tcp, &mut conn)?;
            drop(conn);
            if self.shared.has_pending_tx() {
                self.shared.flush().await?;
            }
            match step {
                Step::Read(processed) => processed.map_err(Error::Tls)?,
                Step::Wait(n) => self.tcp.readable_async(n).await,
            }
        }
    }
}

enum Step {
    Read(core::result::Result<(), devkit_crypto::Error>),
    Wait(usize),
}

#[cfg(all(target_os = "none", CONFIG_SOC_AES_SUPPORT_GCM, CONFIG_SOC_AES_GDMA))]
const RECORD_HEADER: usize = 5;

#[cfg(all(target_os = "none", CONFIG_SOC_AES_SUPPORT_GCM, CONFIG_SOC_AES_GDMA))]
/// The next record straight from the TCP ring, decrypted where it lies and
/// then dropped from it; the lock back when it has to be copied out instead,
/// as a record larger than the ring is.
fn direct<'a>(mut tcp: TcpReadLock<'a>, conn: &mut Connection) -> core::result::Result<Result<Step>, TcpReadLock<'a>> {
    if !conn.takes_records() {
        return Err(tcp);
    }
    let (a, b) = tcp.held();
    let held = a.len() + b.len();
    let byte = |i: usize| if i < a.len() { a[i] } else { b[i - a.len()] };
    let need = if held < RECORD_HEADER {
        RECORD_HEADER
    } else {
        RECORD_HEADER + usize::from(u16::from_be_bytes([byte(3), byte(4)]))
    };
    if need > tcp.capacity() {
        return Err(tcp);
    }
    if held < need {
        return Ok(match tcp.end() {
            None => Ok(Step::Wait(need)),
            Some(Ok(())) => Err(Error::ConnectionAborted),
            Some(Err(err)) => Err(err),
        });
    }
    let first = need.min(a.len());
    let record = [&a[..first], &b[..need - first]];
    let pieces = if need > first { &record[..] } else { &record[..1] };
    let Some(received) = conn.receive_record(pieces) else { return Err(tcp) };
    tcp.discard(need);
    Ok(Ok(Step::Read(received)))
}

/// What the TCP ring holds, copied into the record buffer as far as it goes.
fn copied(mut tcp: TcpReadLock<'_>, conn: &mut Connection) -> Result<Step> {
    let space = conn.rx_space();
    if space.is_empty() {
        return Err(Error::ConnectionAborted);
    }
    match tcp.try_read(space) {
        None => {
            drop(tcp);
            Ok(Step::Wait(conn.rx_wanted()))
        }
        Some(Ok(0)) => Err(Error::ConnectionAborted),
        Some(Ok(got)) => {
            drop(tcp);
            Ok(Step::Read(conn.commit_rx(got)))
        }
        Some(Err(err)) => Err(err),
    }
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
