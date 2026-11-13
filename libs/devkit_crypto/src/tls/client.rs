/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::DerefMut;

use devkit::ByteReader;
use idf::heap::Caps;

use super::schedule::{HashKind, Secret, Transcript};
use super::{CipherSuite, ClientConfig, PrivateKey, Verify, alert};
use crate::aead::{Aead, NONCE, TAG};
use crate::ecdh::{EphemeralSecret, Group};
use crate::ecdsa::{self, Curve};
use crate::hash::HashAlg;
use crate::x509::{self, Certificate, PublicKey, ServerName, SignatureAlgorithm};
use crate::{CertError, Error, Result, ct, rand};

const HEADER: usize = 5;
const MAX_PLAIN: usize = 1 << 14;
const MAX_CIPHER: usize = MAX_PLAIN + 256;
const RX_CAP: usize = HEADER + MAX_CIPHER;
const MAX_HANDSHAKE: usize = 1 << 16;

const CHANGE_CIPHER_SPEC: u8 = 20;
const ALERT: u8 = 21;
const HANDSHAKE: u8 = 22;
const APPLICATION_DATA: u8 = 23;

const CLIENT_HELLO: u8 = 1;
const SERVER_HELLO: u8 = 2;
const NEW_SESSION_TICKET: u8 = 4;
const ENCRYPTED_EXTENSIONS: u8 = 8;
const CERTIFICATE: u8 = 11;
const CERTIFICATE_REQUEST: u8 = 13;
const CERTIFICATE_VERIFY: u8 = 15;
const FINISHED: u8 = 20;
const KEY_UPDATE: u8 = 24;
const MESSAGE_HASH: u8 = 254;

const EXT_SERVER_NAME: u16 = 0;
const EXT_SUPPORTED_GROUPS: u16 = 10;
const EXT_SIGNATURE_ALGORITHMS: u16 = 13;
const EXT_ALPN: u16 = 16;
const EXT_SUPPORTED_VERSIONS: u16 = 43;
const EXT_COOKIE: u16 = 44;
const EXT_KEY_SHARE: u16 = 51;

const TLS13: u16 = 0x0304;

const ECDSA_P256_SHA256: u16 = 0x0403;
const ECDSA_P384_SHA384: u16 = 0x0503;
const ECDSA_P521_SHA512: u16 = 0x0603;
const RSA_PSS_SHA256: u16 = 0x0804;
const RSA_PSS_SHA384: u16 = 0x0805;
const RSA_PSS_SHA512: u16 = 0x0806;
const ED25519: u16 = 0x0807;
const RSA_PKCS1_SHA256: u16 = 0x0401;
const RSA_PKCS1_SHA384: u16 = 0x0501;
const RSA_PKCS1_SHA512: u16 = 0x0601;
/// The first seven may sign a CertificateVerify; PKCS #1 v1.5 only certificates.
const SIGNATURE_SCHEMES: [u16; 10] = [
    ECDSA_P256_SHA256,
    ECDSA_P384_SHA384,
    ECDSA_P521_SHA512,
    RSA_PSS_SHA256,
    RSA_PSS_SHA384,
    RSA_PSS_SHA512,
    ED25519,
    RSA_PKCS1_SHA256,
    RSA_PKCS1_SHA384,
    RSA_PKCS1_SHA512,
];

/// SHA-256 of "HelloRetryRequest", the random a HelloRetryRequest carries.
const HRR_RANDOM: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91, 0xc2, 0xa2, 0x11, 0x16,
    0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

fn group_id(group: Group) -> u16 {
    match group {
        Group::X25519 => 0x001d,
        Group::P256 => 0x0017,
        Group::P384 => 0x0018,
    }
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Writes `f`'s bytes after a big-endian length of `width` bytes.
fn with_len(out: &mut Vec<u8>, width: usize, f: impl FnOnce(&mut Vec<u8>)) {
    let at = out.len();
    out.extend_from_slice(&[0; 3][..width]);
    f(out);
    let len = (out.len() - at - width) as u32;
    out[at..at + width].copy_from_slice(&len.to_be_bytes()[4 - width..]);
}

fn vec8<'a>(r: &mut ByteReader<'a>) -> Result<ByteReader<'a>> {
    let n = r.u8()? as usize;
    Ok(r.sub(n)?)
}

fn vec16<'a>(r: &mut ByteReader<'a>) -> Result<ByteReader<'a>> {
    let n = r.u16_be()? as usize;
    Ok(r.sub(n)?)
}

fn vec24<'a>(r: &mut ByteReader<'a>) -> Result<ByteReader<'a>> {
    let n = r.u24_be()? as usize;
    Ok(r.sub(n)?)
}

/// A failure that has a fatal alert to send.
struct Fatal {
    alert: u8,
    error: Error,
}

type Step<T> = core::result::Result<T, Fatal>;

fn fatal(alert: u8) -> Fatal {
    let error = if alert == super::alert::HANDSHAKE_FAILURE { Error::HandshakeFailure } else { Error::Protocol };
    Fatal { alert, error }
}

impl From<Error> for Fatal {
    fn from(error: Error) -> Self {
        let alert = match error {
            Error::Decode => alert::DECODE_ERROR,
            Error::Certificate(CertError::Expired | CertError::NotYetValid) => alert::CERTIFICATE_EXPIRED,
            Error::Certificate(CertError::UnknownIssuer | CertError::Distrusted) => alert::UNKNOWN_CA,
            Error::Certificate(CertError::UnsupportedAlgorithm) => alert::UNSUPPORTED_CERTIFICATE,
            Error::Certificate(_) => alert::BAD_CERTIFICATE,
            Error::Verify => alert::DECRYPT_ERROR,
            Error::HandshakeFailure => alert::HANDSHAKE_FAILURE,
            _ => alert::INTERNAL_ERROR,
        };
        Fatal { alert, error }
    }
}

struct RecordCipher {
    aead: Aead,
    iv: [u8; NONCE],
    seq: u64,
}

impl RecordCipher {
    fn new(suite: CipherSuite, secret: &Secret) -> Result<Self> {
        let hash = suite.hash();
        let mut key = [0u8; 32];
        let key = &mut key[..suite.aead().key_len()];
        hash.expand_label(secret.as_bytes(), b"key", &[], key);
        let mut iv = [0u8; NONCE];
        hash.expand_label(secret.as_bytes(), b"iv", &[], &mut iv);
        let aead = Aead::new(suite.aead(), key);
        ct::zeroize(key);
        Ok(Self { aead: aead?, iv, seq: 0 })
    }

    fn nonce(&mut self) -> Step<[u8; NONCE]> {
        let mut nonce = self.iv;
        for (n, s) in nonce[4..].iter_mut().zip(self.seq.to_be_bytes()) {
            *n ^= s;
        }
        self.seq = self.seq.checked_add(1).ok_or(fatal(alert::INTERNAL_ERROR))?;
        Ok(nonce)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    ServerHello,
    EncryptedExtensions,
    CertificateOrRequest,
    Certificate,
    CertificateVerify,
    Finished,
    Connected,
}

/// A TLS 1.3 client connection, without I/O: bytes from the peer go in through
/// `rx_space` and `commit_rx` (or `receive`), bytes for the peer come out of
/// `pending_tx`. Writes made during the handshake go out after it.
pub struct Connection(Box<Inner>);

impl Connection {
    /// Starts a handshake; the ClientHello is in `pending_tx`. `server_name`
    /// goes out as SNI when it is a DNS name and is what `Verify::WebPki` checks.
    pub fn client(config: Arc<ClientConfig>, server_name: Option<&str>) -> Result<Self> {
        let mut rx = idf::heap::try_vec(RX_CAP, config.buffer_caps).map_err(|_| Error::NoMemory)?;
        rx.resize(RX_CAP, 0);
        Ok(Self(Box::new(Inner::client(config, server_name, RxBuf::Heap(rx))?)))
    }

    /// The receive buffer `client_in` needs.
    pub const RX_BYTES: usize = RX_CAP;

    /// As `client`, receiving in `rx` instead of `buffer_caps` memory; `rx`
    /// holds at least `RX_BYTES`.
    pub fn client_in(config: Arc<ClientConfig>, server_name: Option<&str>, rx: Box<dyn DerefMut<Target = [u8]> + Send>) -> Result<Self> {
        if rx.len() < RX_CAP {
            return Err(Error::NoMemory);
        }
        Ok(Self(Box::new(Inner::client(config, server_name, RxBuf::Given(rx))?)))
    }

    pub fn is_handshaking(&self) -> bool {
        self.0.is_handshaking()
    }

    pub fn pending_tx(&self) -> &[u8] {
        self.0.pending_tx()
    }

    pub fn consume_tx(&mut self, n: usize) {
        self.0.consume_tx(n)
    }

    /// Room for bytes from the peer; never empty while the connection works.
    pub fn rx_space(&mut self) -> &mut [u8] {
        self.0.rx_space()
    }

    /// Takes `n` bytes written to `rx_space` and processes what they complete.
    pub fn commit_rx(&mut self, n: usize) -> Result<()> {
        self.0.commit_rx(n)
    }

    /// Copies in as much of `data` as fits; returns how much.
    pub fn receive(&mut self, data: &[u8]) -> Result<usize> {
        self.0.receive(data)
    }

    /// Application data received so far; 0 when there is none yet, or none
    /// will come (`peer_closed`).
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        self.0.read(out)
    }

    /// Encrypts `data` into `pending_tx`, or holds it until the handshake ends.
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.0.write(data)
    }

    /// Queues close_notify; nothing can be written after it.
    pub fn close(&mut self) {
        self.0.close()
    }

    pub fn peer_closed(&self) -> bool {
        self.0.peer_closed()
    }

    /// The error that ended the connection, if one did.
    pub fn error(&self) -> Option<Error> {
        self.0.error()
    }

    pub fn alpn(&self) -> Option<&[u8]> {
        self.0.alpn()
    }

    pub fn cipher_suite(&self) -> Option<CipherSuite> {
        self.0.cipher_suite()
    }

    /// The server's chain, leaf first, once it arrived.
    pub fn peer_certificates(&self) -> &[Vec<u8>] {
        self.0.peer_certificates()
    }

    /// RFC 8446 section 7.5; an absent context is the same as an empty one.
    pub fn export_keying_material(&self, label: &[u8], context: &[u8], out: &mut [u8]) -> Result<()> {
        self.0.export_keying_material(label, context, out)
    }
}

struct Inner {
    config: Arc<ClientConfig>,
    server_name: Option<String>,
    state: State,
    failed: Option<Error>,
    rx: RxBuf,
    rx_len: usize,
    /// Decrypted application data within `rx`, and where its record ends.
    app: (usize, usize, usize),
    tx: Vec<u8>,
    tx_pos: usize,
    early_writes: Vec<u8>,
    handshake: Vec<u8>,
    read_cipher: Option<RecordCipher>,
    write_cipher: Option<RecordCipher>,
    suite: Option<CipherSuite>,
    hash: HashKind,
    transcript: Option<Transcript>,
    client_hello: Vec<u8>,
    session_id: [u8; 32],
    random: [u8; 32],
    share: Option<EphemeralSecret>,
    retried: bool,
    handshake_secret: Option<Secret>,
    client_secret: Option<Secret>,
    server_secret: Option<Secret>,
    exporter: Option<Secret>,
    certificate_requested: Option<(Vec<u8>, Vec<u16>)>,
    peer_chain: Vec<Vec<u8>>,
    peer_key: Option<PublicKey>,
    alpn: Option<Vec<u8>>,
    peer_closed: bool,
    closing: bool,
}

enum RxBuf {
    Heap(Vec<u8, Caps>),
    Given(Box<dyn DerefMut<Target = [u8]> + Send>),
}

impl core::ops::Deref for RxBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Heap(v) => v,
            Self::Given(g) => &g[..RX_CAP],
        }
    }
}

impl DerefMut for RxBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Heap(v) => v,
            Self::Given(g) => &mut g[..RX_CAP],
        }
    }
}

impl Inner {
    fn client(config: Arc<ClientConfig>, server_name: Option<&str>, rx: RxBuf) -> Result<Self> {
        if config.cipher_suites.is_empty() || config.groups.is_empty() {
            return Err(Error::HandshakeFailure);
        }
        let mut session_id = [0; 32];
        let mut random = [0; 32];
        rand::fill(&mut session_id);
        rand::fill(&mut random);
        let share = EphemeralSecret::generate(config.groups[0])?;
        let mut conn = Self {
            config,
            server_name: server_name.map(String::from),
            state: State::ServerHello,
            failed: None,
            rx,
            rx_len: 0,
            app: (0, 0, 0),
            tx: Vec::new(),
            tx_pos: 0,
            early_writes: Vec::new(),
            handshake: Vec::new(),
            read_cipher: None,
            write_cipher: None,
            suite: None,
            hash: HashKind::Sha256,
            transcript: None,
            client_hello: Vec::new(),
            session_id,
            random,
            share: Some(share),
            retried: false,
            handshake_secret: None,
            client_secret: None,
            server_secret: None,
            exporter: None,
            certificate_requested: None,
            peer_chain: Vec::new(),
            peer_key: None,
            alpn: None,
            peer_closed: false,
            closing: false,
        };
        let hello = conn.client_hello(None)?;
        conn.write_plain(HANDSHAKE, &hello, 0x0301);
        conn.client_hello = hello;
        Ok(conn)
    }

    pub fn is_handshaking(&self) -> bool {
        self.state != State::Connected && self.failed.is_none()
    }

    pub fn pending_tx(&self) -> &[u8] {
        &self.tx[self.tx_pos..]
    }

    pub fn consume_tx(&mut self, n: usize) {
        self.tx_pos = (self.tx_pos + n).min(self.tx.len());
        if self.tx_pos == self.tx.len() {
            self.tx.clear();
            self.tx_pos = 0;
        }
    }

    pub fn rx_space(&mut self) -> &mut [u8] {
        &mut self.rx[self.rx_len..]
    }

    pub fn commit_rx(&mut self, n: usize) -> Result<()> {
        self.rx_len = (self.rx_len + n).min(RX_CAP);
        self.process()
    }

    pub fn receive(&mut self, data: &[u8]) -> Result<usize> {
        let space = self.rx_space();
        let n = space.len().min(data.len());
        space[..n].copy_from_slice(&data[..n]);
        self.commit_rx(n)?;
        Ok(n)
    }

    pub fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        if self.app.0 == self.app.1 {
            self.process()?;
        }
        if let Some(e) = self.failed {
            if self.app.0 == self.app.1 {
                return Err(e);
            }
        }
        let (start, end, _) = self.app;
        let n = out.len().min(end - start);
        out[..n].copy_from_slice(&self.rx[start..start + n]);
        self.app.0 += n;
        if self.app.0 == self.app.1 {
            self.drop_record(self.app.2);
            self.app = (0, 0, 0);
        }
        Ok(n)
    }

    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        if self.closing {
            return Err(Error::Closed);
        }
        if self.state != State::Connected {
            self.early_writes.extend_from_slice(data);
            return Ok(());
        }
        for chunk in data.chunks(MAX_PLAIN) {
            self.write_record(APPLICATION_DATA, chunk).map_err(|f| f.error)?;
        }
        Ok(())
    }

    pub fn close(&mut self) {
        if !self.closing && self.failed.is_none() {
            self.closing = true;
            let _ = self.write_record(ALERT, &[1, alert::CLOSE_NOTIFY]);
        }
    }

    pub fn peer_closed(&self) -> bool {
        self.peer_closed
    }

    pub fn error(&self) -> Option<Error> {
        self.failed
    }

    pub fn alpn(&self) -> Option<&[u8]> {
        self.alpn.as_deref()
    }

    pub fn cipher_suite(&self) -> Option<CipherSuite> {
        self.suite
    }

    pub fn peer_certificates(&self) -> &[Vec<u8>] {
        &self.peer_chain
    }

    pub fn export_keying_material(&self, label: &[u8], context: &[u8], out: &mut [u8]) -> Result<()> {
        let exporter = self.exporter.as_ref().ok_or(Error::Closed)?;
        let hash = self.hash;
        let secret = hash.derive(exporter, label, &hash.empty_hash());
        hash.expand_label(secret.as_bytes(), b"exporter", hash.digest(context).as_bytes(), out);
        Ok(())
    }

    fn client_hello(&mut self, cookie: Option<&[u8]>) -> Result<Vec<u8>> {
        let share = self.share.as_ref().unwrap();
        let public = share.public_key()?;
        let mut msg = Vec::with_capacity(512);
        msg.push(CLIENT_HELLO);
        with_len(&mut msg, 3, |m| {
            put_u16(m, 0x0303);
            m.extend_from_slice(&self.random);
            m.push(32);
            m.extend_from_slice(&self.session_id);
            with_len(m, 2, |m| {
                for suite in &self.config.cipher_suites {
                    put_u16(m, suite.id());
                }
            });
            m.extend_from_slice(&[1, 0]);
            with_len(m, 2, |m| {
                if let Some(name) = &self.server_name {
                    if let ServerName::Dns(host) = ServerName::parse(name) {
                        put_u16(m, EXT_SERVER_NAME);
                        with_len(m, 2, |m| {
                            with_len(m, 2, |m| {
                                m.push(0);
                                with_len(m, 2, |m| m.extend_from_slice(host.as_bytes()));
                            })
                        });
                    }
                }
                put_u16(m, EXT_SUPPORTED_VERSIONS);
                with_len(m, 2, |m| with_len(m, 1, |m| put_u16(m, TLS13)));
                put_u16(m, EXT_SUPPORTED_GROUPS);
                with_len(m, 2, |m| {
                    with_len(m, 2, |m| {
                        for g in &self.config.groups {
                            put_u16(m, group_id(*g));
                        }
                    })
                });
                put_u16(m, EXT_SIGNATURE_ALGORITHMS);
                with_len(m, 2, |m| {
                    with_len(m, 2, |m| {
                        for s in SIGNATURE_SCHEMES {
                            put_u16(m, s);
                        }
                    })
                });
                put_u16(m, EXT_KEY_SHARE);
                with_len(m, 2, |m| {
                    with_len(m, 2, |m| {
                        put_u16(m, group_id(share.group()));
                        with_len(m, 2, |m| m.extend_from_slice(&public));
                    })
                });
                if !self.config.alpn.is_empty() {
                    put_u16(m, EXT_ALPN);
                    with_len(m, 2, |m| {
                        with_len(m, 2, |m| {
                            for proto in &self.config.alpn {
                                with_len(m, 1, |m| m.extend_from_slice(proto));
                            }
                        })
                    });
                }
                if let Some(cookie) = cookie {
                    put_u16(m, EXT_COOKIE);
                    with_len(m, 2, |m| with_len(m, 2, |m| m.extend_from_slice(cookie)));
                }
            });
        });
        Ok(msg)
    }

    fn write_plain(&mut self, ty: u8, payload: &[u8], version: u16) {
        self.tx.push(ty);
        put_u16(&mut self.tx, version);
        put_u16(&mut self.tx, payload.len() as u16);
        self.tx.extend_from_slice(payload);
    }

    fn write_record(&mut self, ty: u8, payload: &[u8]) -> Step<()> {
        let Some(cipher) = self.write_cipher.as_mut() else {
            for chunk in payload.chunks(MAX_PLAIN) {
                self.write_plain(ty, chunk, 0x0303);
            }
            return Ok(());
        };
        for chunk in payload.chunks(MAX_PLAIN) {
            let inner = chunk.len() + 1;
            let header = [APPLICATION_DATA, 3, 3, ((inner + TAG) >> 8) as u8, (inner + TAG) as u8];
            let at = self.tx.len();
            self.tx.extend_from_slice(&header);
            self.tx.extend_from_slice(chunk);
            self.tx.push(ty);
            self.tx.extend_from_slice(&[0; TAG]);
            let nonce = cipher.nonce()?;
            let (head, body) = self.tx[at..].split_at_mut(HEADER);
            cipher.aead.seal_in_place(&nonce, head, body, inner).map_err(|_| fatal(alert::INTERNAL_ERROR))?;
        }
        Ok(())
    }

    fn send_handshake(&mut self, msg: &[u8]) -> Step<()> {
        self.transcript.as_mut().unwrap().update(msg);
        self.write_record(HANDSHAKE, msg)
    }

    fn fail(&mut self, f: Fatal) -> Error {
        if self.failed.is_none() {
            if f.alert != alert::CLOSE_NOTIFY && !matches!(f.error, Error::Alert(_)) {
                let _ = self.write_record(ALERT, &[2, f.alert]);
            }
            self.failed = Some(f.error);
        }
        f.error
    }

    fn drop_record(&mut self, end: usize) {
        self.rx.copy_within(end..self.rx_len, 0);
        self.rx_len -= end;
    }

    fn process(&mut self) -> Result<()> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        while self.app.0 == self.app.1 && !self.peer_closed {
            match self.next_record() {
                Ok(true) => {}
                Ok(false) => break,
                Err(f) => return Err(self.fail(f)),
            }
        }
        Ok(())
    }

    /// Handles one whole record; `false` when none is buffered.
    fn next_record(&mut self) -> Step<bool> {
        if self.rx_len < HEADER {
            return Ok(false);
        }
        let ty = self.rx[0];
        let len = u16::from_be_bytes([self.rx[3], self.rx[4]]) as usize;
        if len > MAX_CIPHER {
            return Err(fatal(alert::RECORD_OVERFLOW));
        }
        if self.rx_len < HEADER + len {
            return Ok(false);
        }
        let end = HEADER + len;
        if ty == CHANGE_CIPHER_SPEC {
            if self.state == State::Connected || len != 1 || self.rx[HEADER] != 1 {
                return Err(fatal(alert::UNEXPECTED_MESSAGE));
            }
            self.drop_record(end);
            return Ok(true);
        }
        let (content_type, start, plain_end) = match self.read_cipher.as_mut() {
            None => {
                if ty == APPLICATION_DATA {
                    return Err(fatal(alert::UNEXPECTED_MESSAGE));
                }
                (ty, HEADER, end)
            }
            Some(cipher) => {
                if ty != APPLICATION_DATA {
                    return Err(fatal(alert::UNEXPECTED_MESSAGE));
                }
                let nonce = cipher.nonce()?;
                let (head, body) = self.rx[..end].split_at_mut(HEADER);
                let plain = cipher.aead.open_in_place(&nonce, head, body).map_err(|_| fatal(alert::BAD_RECORD_MAC))?;
                let Some(last) = plain.iter().rposition(|&b| b != 0) else {
                    return Err(fatal(alert::UNEXPECTED_MESSAGE));
                };
                if last > MAX_PLAIN {
                    return Err(fatal(alert::RECORD_OVERFLOW));
                }
                (plain[last], HEADER, HEADER + last)
            }
        };
        if content_type != HANDSHAKE && !self.handshake.is_empty() {
            return Err(fatal(alert::UNEXPECTED_MESSAGE));
        }
        match content_type {
            HANDSHAKE => {
                if start == plain_end {
                    return Err(fatal(alert::UNEXPECTED_MESSAGE));
                }
                let data = self.rx[start..plain_end].to_vec();
                self.drop_record(end);
                self.handshake_data(&data)?;
            }
            ALERT => {
                let body = &self.rx[start..plain_end];
                if body.len() != 2 {
                    return Err(fatal(alert::DECODE_ERROR));
                }
                let code = body[1];
                self.drop_record(end);
                if code == alert::CLOSE_NOTIFY {
                    self.peer_closed = true;
                } else {
                    let error = if code == alert::USER_CANCELED { Error::Closed } else { Error::Alert(code) };
                    return Err(Fatal { alert: alert::CLOSE_NOTIFY, error });
                }
            }
            APPLICATION_DATA => {
                if self.state != State::Connected {
                    return Err(fatal(alert::UNEXPECTED_MESSAGE));
                }
                if start == plain_end {
                    self.drop_record(end);
                } else {
                    self.app = (start, plain_end, end);
                }
            }
            _ => return Err(fatal(alert::UNEXPECTED_MESSAGE)),
        }
        Ok(true)
    }

    fn handshake_data(&mut self, data: &[u8]) -> Step<()> {
        self.handshake.extend_from_slice(data);
        loop {
            if self.handshake.len() < 4 {
                return Ok(());
            }
            let len = u32::from_be_bytes([0, self.handshake[1], self.handshake[2], self.handshake[3]]) as usize;
            if len > MAX_HANDSHAKE {
                return Err(fatal(alert::DECODE_ERROR));
            }
            if self.handshake.len() < 4 + len {
                return Ok(());
            }
            let msg: Vec<u8> = self.handshake.drain(..4 + len).collect();
            let keys_change = self.message(&msg)?;
            if keys_change && !self.handshake.is_empty() {
                return Err(fatal(alert::UNEXPECTED_MESSAGE));
            }
        }
    }

    /// Handles one handshake message; `true` when it changed the read keys.
    fn message(&mut self, msg: &[u8]) -> Step<bool> {
        let ty = msg[0];
        let body = &msg[4..];
        match (self.state, ty) {
            (State::ServerHello, SERVER_HELLO) => self.server_hello(msg),
            (State::EncryptedExtensions, ENCRYPTED_EXTENSIONS) => {
                self.encrypted_extensions(body)?;
                self.transcript.as_mut().unwrap().update(msg);
                self.state = State::CertificateOrRequest;
                Ok(false)
            }
            (State::CertificateOrRequest, CERTIFICATE_REQUEST) => {
                self.certificate_request(body)?;
                self.transcript.as_mut().unwrap().update(msg);
                self.state = State::Certificate;
                Ok(false)
            }
            (State::CertificateOrRequest | State::Certificate, CERTIFICATE) => {
                self.certificate(body)?;
                self.transcript.as_mut().unwrap().update(msg);
                self.state = State::CertificateVerify;
                Ok(false)
            }
            (State::CertificateVerify, CERTIFICATE_VERIFY) => {
                self.certificate_verify(body)?;
                self.transcript.as_mut().unwrap().update(msg);
                self.state = State::Finished;
                Ok(false)
            }
            (State::Finished, FINISHED) => {
                self.finished(msg)?;
                Ok(true)
            }
            (State::Connected, NEW_SESSION_TICKET) => Ok(false),
            (State::Connected, KEY_UPDATE) => {
                self.key_update(body)?;
                Ok(true)
            }
            _ => Err(fatal(alert::UNEXPECTED_MESSAGE)),
        }
    }

    fn server_hello(&mut self, msg: &[u8]) -> Step<bool> {
        let mut r = ByteReader::new(&msg[4..]);
        let decode = |_| fatal(alert::DECODE_ERROR);
        if r.u16_be().map_err(decode)? != 0x0303 {
            return Err(fatal(alert::PROTOCOL_VERSION));
        }
        let random: [u8; 32] = r.array().map_err(decode)?;
        let session = vec8(&mut r).map_err(|_| fatal(alert::DECODE_ERROR))?;
        let suite_id = r.u16_be().map_err(decode)?;
        let compression = r.u8().map_err(decode)?;
        let mut exts = vec16(&mut r).map_err(|_| fatal(alert::DECODE_ERROR))?;
        if !r.is_empty() {
            return Err(fatal(alert::DECODE_ERROR));
        }
        if session.rest() != self.session_id || compression != 0 {
            return Err(fatal(alert::ILLEGAL_PARAMETER));
        }
        let suite = CipherSuite::from_id(suite_id)
            .filter(|s| self.config.cipher_suites.contains(s))
            .ok_or(fatal(alert::ILLEGAL_PARAMETER))?;
        if self.suite.is_some_and(|s| s != suite) {
            return Err(fatal(alert::ILLEGAL_PARAMETER));
        }
        let retry = random == HRR_RANDOM;
        let mut version = None;
        let mut key_share = None;
        let mut selected_group = None;
        let mut cookie = None;
        while !exts.is_empty() {
            let id = exts.u16_be().map_err(decode)?;
            let mut data = vec16(&mut exts).map_err(|_| fatal(alert::DECODE_ERROR))?;
            match id {
                EXT_SUPPORTED_VERSIONS => version = Some(data.u16_be().map_err(decode)?),
                EXT_KEY_SHARE if retry => selected_group = Some(data.u16_be().map_err(decode)?),
                EXT_KEY_SHARE => {
                    let group = data.u16_be().map_err(decode)?;
                    let key = vec16(&mut data).map_err(|_| fatal(alert::DECODE_ERROR))?;
                    key_share = Some((group, key.rest().to_vec()));
                }
                EXT_COOKIE if retry => cookie = Some(vec16(&mut data).map_err(|_| fatal(alert::DECODE_ERROR))?.rest().to_vec()),
                _ => return Err(fatal(alert::UNSUPPORTED_EXTENSION)),
            }
        }
        if version != Some(TLS13) {
            return Err(fatal(alert::PROTOCOL_VERSION));
        }
        self.suite = Some(suite);
        self.hash = suite.hash();
        if self.transcript.is_none() {
            let mut t = Transcript::new(self.hash);
            t.update(&self.client_hello);
            self.transcript = Some(t);
        }
        if retry {
            return self.hello_retry(msg, selected_group, cookie);
        }
        let share = self.share.take().unwrap();
        let (group, peer) = key_share.ok_or(fatal(alert::MISSING_EXTENSION))?;
        if group != group_id(share.group()) {
            return Err(fatal(alert::ILLEGAL_PARAMETER));
        }
        let shared = share.agree(&peer).map_err(|_| fatal(alert::ILLEGAL_PARAMETER))?;
        if shared.iter().all(|&b| b == 0) {
            return Err(fatal(alert::ILLEGAL_PARAMETER));
        }
        let transcript = self.transcript.as_mut().unwrap();
        transcript.update(msg);
        let hash = self.hash;
        let zeros = [0u8; 48];
        let early = hash.extract(&[], &zeros[..hash.len()]);
        let derived = hash.derive(&early, b"derived", &hash.empty_hash());
        let handshake = hash.extract(derived.as_bytes(), &shared);
        let th = transcript.current();
        let client = hash.derive(&handshake, b"c hs traffic", &th);
        let server = hash.derive(&handshake, b"s hs traffic", &th);
        self.read_cipher = Some(RecordCipher::new(suite, &server)?);
        self.write_cipher = Some(RecordCipher::new(suite, &client)?);
        self.handshake_secret = Some(handshake);
        self.client_secret = Some(client);
        self.server_secret = Some(server);
        self.state = State::EncryptedExtensions;
        Ok(true)
    }

    fn hello_retry(&mut self, msg: &[u8], group: Option<u16>, cookie: Option<Vec<u8>>) -> Step<bool> {
        if self.retried {
            return Err(fatal(alert::UNEXPECTED_MESSAGE));
        }
        self.retried = true;
        let group_id = group.ok_or(fatal(alert::MISSING_EXTENSION))?;
        let group = self
            .config
            .groups
            .iter()
            .copied()
            .find(|&g| self::group_id(g) == group_id)
            .filter(|&g| Some(g) != self.share.as_ref().map(|s| s.group()))
            .ok_or(fatal(alert::ILLEGAL_PARAMETER))?;
        let first = self.hash.digest(&self.client_hello);
        let mut t = Transcript::new(self.hash);
        t.update(&[MESSAGE_HASH, 0, 0, self.hash.len() as u8]);
        t.update(first.as_bytes());
        t.update(msg);
        self.transcript = Some(t);
        self.share = Some(EphemeralSecret::generate(group)?);
        let hello = self.client_hello(cookie.as_deref())?;
        self.transcript.as_mut().unwrap().update(&hello);
        self.write_plain(CHANGE_CIPHER_SPEC, &[1], 0x0303);
        self.write_plain(HANDSHAKE, &hello, 0x0303);
        Ok(false)
    }

    fn encrypted_extensions(&mut self, body: &[u8]) -> Step<()> {
        let mut r = ByteReader::new(body);
        let mut exts = vec16(&mut r).map_err(|_| fatal(alert::DECODE_ERROR))?;
        if !r.is_empty() {
            return Err(fatal(alert::DECODE_ERROR));
        }
        while !exts.is_empty() {
            let id = exts.u16_be().map_err(|_| fatal(alert::DECODE_ERROR))?;
            let mut data = vec16(&mut exts).map_err(|_| fatal(alert::DECODE_ERROR))?;
            match id {
                EXT_SERVER_NAME | EXT_SUPPORTED_GROUPS => {}
                EXT_ALPN if !self.config.alpn.is_empty() => {
                    let mut list = vec16(&mut data).map_err(|_| fatal(alert::DECODE_ERROR))?;
                    let proto = vec8(&mut list).map_err(|_| fatal(alert::DECODE_ERROR))?.rest().to_vec();
                    if !list.is_empty() || !self.config.alpn.contains(&proto) {
                        return Err(fatal(alert::ILLEGAL_PARAMETER));
                    }
                    self.alpn = Some(proto);
                }
                _ => return Err(fatal(alert::UNSUPPORTED_EXTENSION)),
            }
        }
        Ok(())
    }

    fn certificate_request(&mut self, body: &[u8]) -> Step<()> {
        let mut r = ByteReader::new(body);
        let context = vec8(&mut r).map_err(|_| fatal(alert::DECODE_ERROR))?.rest().to_vec();
        let mut exts = vec16(&mut r).map_err(|_| fatal(alert::DECODE_ERROR))?;
        if !context.is_empty() {
            return Err(fatal(alert::ILLEGAL_PARAMETER));
        }
        let mut schemes = Vec::new();
        while !exts.is_empty() {
            let id = exts.u16_be().map_err(|_| fatal(alert::DECODE_ERROR))?;
            let mut data = vec16(&mut exts).map_err(|_| fatal(alert::DECODE_ERROR))?;
            if id == EXT_SIGNATURE_ALGORITHMS {
                let mut list = vec16(&mut data).map_err(|_| fatal(alert::DECODE_ERROR))?;
                while !list.is_empty() {
                    schemes.push(list.u16_be().map_err(|_| fatal(alert::DECODE_ERROR))?);
                }
            }
        }
        if schemes.is_empty() {
            return Err(fatal(alert::MISSING_EXTENSION));
        }
        self.certificate_requested = Some((context, schemes));
        Ok(())
    }

    fn certificate(&mut self, body: &[u8]) -> Step<()> {
        let mut r = ByteReader::new(body);
        let context = vec8(&mut r).map_err(|_| fatal(alert::DECODE_ERROR))?;
        let mut list = vec24(&mut r).map_err(|_| fatal(alert::DECODE_ERROR))?;
        if !context.is_empty() || !r.is_empty() {
            return Err(fatal(alert::DECODE_ERROR));
        }
        let mut chain = Vec::new();
        while !list.is_empty() {
            let cert = vec24(&mut list).map_err(|_| fatal(alert::DECODE_ERROR))?;
            vec16(&mut list).map_err(|_| fatal(alert::DECODE_ERROR))?;
            chain.push(cert.rest().to_vec());
        }
        if chain.is_empty() {
            return Err(fatal(alert::DECODE_ERROR));
        }
        let leaf = Certificate::parse(&chain[0])?;
        let key = leaf.public_key()?;
        let name = self.server_name.as_deref();
        match &self.config.verify {
            Verify::WebPki(store) => {
                let refs: Vec<&[u8]> = chain.iter().map(|c| c.as_slice()).collect();
                let now = self.config.clock.map(|clock| clock());
                let server = name.map(ServerName::parse);
                x509::verify_chain(&refs, store, now, server.as_ref())?;
            }
            Verify::PinnedSpki(spki) => {
                if leaf.spki != spki.as_slice() {
                    return Err(Error::Certificate(CertError::UnknownIssuer).into());
                }
            }
            Verify::Custom(verifier) => verifier.verify(&chain, name)?,
            Verify::None => {}
        }
        self.peer_key = Some(key);
        self.peer_chain = chain;
        Ok(())
    }

    fn certificate_verify(&mut self, body: &[u8]) -> Step<()> {
        let mut r = ByteReader::new(body);
        let scheme = r.u16_be().map_err(|_| fatal(alert::DECODE_ERROR))?;
        let signature = vec16(&mut r).map_err(|_| fatal(alert::DECODE_ERROR))?.rest();
        if !r.is_empty() {
            return Err(fatal(alert::DECODE_ERROR));
        }
        let key = self.peer_key.as_ref().unwrap();
        let alg = verify_algorithm(scheme, key).ok_or(fatal(alert::ILLEGAL_PARAMETER))?;
        let content = signed_content(b"TLS 1.3, server CertificateVerify", &self.transcript.as_ref().unwrap().current());
        key.verify(alg, &content, signature).map_err(|_| fatal(alert::DECRYPT_ERROR))
    }

    fn finished(&mut self, msg: &[u8]) -> Step<()> {
        let hash = self.hash;
        let transcript = self.transcript.as_mut().unwrap();
        let server = self.server_secret.take().unwrap();
        let key = hash.expand_secret(server.as_bytes(), b"finished", &[]);
        let expected = hash.hmac(key.as_bytes(), transcript.current().as_bytes());
        if !ct::eq(expected.as_bytes(), &msg[4..]) {
            return Err(fatal(alert::DECRYPT_ERROR));
        }
        transcript.update(msg);
        let th = transcript.current();
        let handshake = self.handshake_secret.take().unwrap();
        let zeros = [0u8; 48];
        let derived = hash.derive(&handshake, b"derived", &hash.empty_hash());
        let master = hash.extract(derived.as_bytes(), &zeros[..hash.len()]);
        let client_app = hash.derive(&master, b"c ap traffic", &th);
        let server_app = hash.derive(&master, b"s ap traffic", &th);
        self.exporter = Some(hash.derive(&master, b"exp master", &th));
        let suite = self.suite.unwrap();
        self.read_cipher = Some(RecordCipher::new(suite, &server_app)?);
        self.server_secret = Some(server_app);

        if !self.retried {
            self.write_plain(CHANGE_CIPHER_SPEC, &[1], 0x0303);
        }
        if let Some((context, schemes)) = self.certificate_requested.take() {
            self.client_certificate(&context, &schemes)?;
        }
        let client = self.client_secret.take().unwrap();
        let key = hash.expand_secret(client.as_bytes(), b"finished", &[]);
        let verify = hash.hmac(key.as_bytes(), self.transcript.as_ref().unwrap().current().as_bytes());
        let mut fin = alloc::vec![FINISHED];
        with_len(&mut fin, 3, |m| m.extend_from_slice(verify.as_bytes()));
        self.send_handshake(&fin)?;
        self.write_cipher = Some(RecordCipher::new(suite, &client_app)?);
        self.client_secret = Some(client_app);
        self.state = State::Connected;
        let early = core::mem::take(&mut self.early_writes);
        for chunk in early.chunks(MAX_PLAIN) {
            self.write_record(APPLICATION_DATA, chunk)?;
        }
        Ok(())
    }

    fn client_certificate(&mut self, context: &[u8], schemes: &[u16]) -> Step<()> {
        let config = self.config.clone();
        let identity = config.identity.as_ref();
        let mut msg = alloc::vec![CERTIFICATE];
        with_len(&mut msg, 3, |m| {
            with_len(m, 1, |m| m.extend_from_slice(context));
            with_len(m, 3, |m| {
                for cert in identity.map_or(&[][..], |i| i.chain.as_slice()) {
                    with_len(m, 3, |m| m.extend_from_slice(cert));
                    put_u16(m, 0);
                }
            });
        });
        self.send_handshake(&msg)?;
        let Some(identity) = identity.filter(|i| !i.chain.is_empty()) else { return Ok(()) };
        let scheme = match &identity.key {
            PrivateKey::Rsa(_) => [RSA_PSS_SHA256, RSA_PSS_SHA384, RSA_PSS_SHA512].into_iter().find(|s| schemes.contains(s)),
            PrivateKey::Ecdsa(key) => Some(match key.curve() {
                Curve::P256 => ECDSA_P256_SHA256,
                Curve::P384 => ECDSA_P384_SHA384,
                Curve::P521 => ECDSA_P521_SHA512,
            }),
            PrivateKey::Ed25519(_) => Some(ED25519),
        };
        let scheme = scheme.filter(|s| schemes.contains(s)).ok_or(fatal(alert::HANDSHAKE_FAILURE))?;
        let content = signed_content(b"TLS 1.3, client CertificateVerify", &self.transcript.as_ref().unwrap().current());
        let signature = match &identity.key {
            PrivateKey::Rsa(key) => {
                let hash = match scheme {
                    RSA_PSS_SHA256 => HashAlg::Sha256,
                    RSA_PSS_SHA384 => HashAlg::Sha384,
                    _ => HashAlg::Sha512,
                };
                key.sign_pss(hash, &content)?
            }
            PrivateKey::Ecdsa(key) => ecdsa::signature_to_der(&key.sign(key.curve().hash(), &content)?),
            PrivateKey::Ed25519(key) => key.sign(&content).to_vec(),
        };
        let mut msg = alloc::vec![CERTIFICATE_VERIFY];
        with_len(&mut msg, 3, |m| {
            put_u16(m, scheme);
            with_len(m, 2, |m| m.extend_from_slice(&signature));
        });
        self.send_handshake(&msg)
    }

    fn key_update(&mut self, body: &[u8]) -> Step<()> {
        let requested = match body {
            [0] => false,
            [1] => true,
            _ => return Err(fatal(alert::DECODE_ERROR)),
        };
        let hash = self.hash;
        let suite = self.suite.unwrap();
        let server = self.server_secret.take().unwrap();
        let next = hash.expand_secret(server.as_bytes(), b"traffic upd", &[]);
        self.read_cipher = Some(RecordCipher::new(suite, &next)?);
        self.server_secret = Some(next);
        if requested && !self.closing {
            self.write_record(HANDSHAKE, &[KEY_UPDATE, 0, 0, 1, 0])?;
            let client = self.client_secret.take().unwrap();
            let next = hash.expand_secret(client.as_bytes(), b"traffic upd", &[]);
            self.write_cipher = Some(RecordCipher::new(suite, &next)?);
            self.client_secret = Some(next);
        }
        Ok(())
    }
}

fn signed_content(context: &[u8], transcript: &Secret) -> Vec<u8> {
    let mut content = alloc::vec![0x20; 64];
    content.extend_from_slice(context);
    content.push(0);
    content.extend_from_slice(transcript.as_bytes());
    content
}

fn verify_algorithm(scheme: u16, key: &PublicKey) -> Option<SignatureAlgorithm> {
    match (scheme, key) {
        (ECDSA_P256_SHA256, PublicKey::Ecdsa(k)) if k.curve() == Curve::P256 => Some(SignatureAlgorithm::Ecdsa(HashAlg::Sha256)),
        (ECDSA_P384_SHA384, PublicKey::Ecdsa(k)) if k.curve() == Curve::P384 => Some(SignatureAlgorithm::Ecdsa(HashAlg::Sha384)),
        (ECDSA_P521_SHA512, PublicKey::Ecdsa(k)) if k.curve() == Curve::P521 => Some(SignatureAlgorithm::Ecdsa(HashAlg::Sha512)),
        (RSA_PSS_SHA256, PublicKey::Rsa(_)) => Some(SignatureAlgorithm::RsaPss(HashAlg::Sha256)),
        (RSA_PSS_SHA384, PublicKey::Rsa(_)) => Some(SignatureAlgorithm::RsaPss(HashAlg::Sha384)),
        (RSA_PSS_SHA512, PublicKey::Rsa(_)) => Some(SignatureAlgorithm::RsaPss(HashAlg::Sha512)),
        (ED25519, PublicKey::Ed25519(_)) => Some(SignatureAlgorithm::Ed25519),
        _ => None,
    }
}
