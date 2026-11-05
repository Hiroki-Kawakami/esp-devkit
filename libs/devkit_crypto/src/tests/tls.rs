/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ecdh::Group;
use crate::tls::{CipherSuite, ClientConfig, Connection, Identity, PrivateKey, Verify};
use crate::x509::{Certificate, CertificateBuilder, Name, Signer, TrustStore};
use crate::{CertError, Error, ed25519};

use super::fixture;

fn port(name: &str) -> u16 {
    let servers = String::from_utf8(fixture("servers.txt")).unwrap();
    let line = servers.lines().find(|l| l.split(' ').next() == Some(name)).unwrap();
    line.split(' ').nth(1).unwrap().parse().unwrap()
}

fn web_pki() -> ClientConfig {
    let mut store = TrustStore::new();
    store.add_der(&fixture("root.der")).unwrap();
    ClientConfig::new(Verify::WebPki(store))
}

struct Session {
    tcp: TcpStream,
    conn: Connection,
}

impl Session {
    fn open(server: &str, config: ClientConfig, name: &str) -> Result<Self, Error> {
        let tcp = TcpStream::connect(("127.0.0.1", port(server))).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let conn = Connection::client(Arc::new(config), Some(name))?;
        let mut session = Session { tcp, conn };
        while session.conn.is_handshaking() {
            session.pump()?;
        }
        session.flush();
        Ok(session)
    }

    fn flush(&mut self) {
        let n = self.conn.pending_tx().len();
        if n > 0 {
            self.tcp.write_all(self.conn.pending_tx()).unwrap();
            self.conn.consume_tx(n);
        }
    }

    /// Sends what is pending, then reads once; `false` at the end of the stream.
    fn pump(&mut self) -> Result<bool, Error> {
        self.flush();
        let n = match self.tcp.read(self.conn.rx_space()) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::ConnectionReset => 0,
            Err(e) => panic!("socket: {e}"),
        };
        let result = self.conn.commit_rx(n);
        self.flush();
        result?;
        Ok(n > 0)
    }

    fn read_to_end(&mut self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = self.conn.read(&mut buf)?;
            out.extend_from_slice(&buf[..n]);
            if n == 0 && (self.conn.peer_closed() || !self.pump()?) {
                return Ok(out);
            }
        }
    }

    fn get_bytes(mut self, path: &str) -> Result<Vec<u8>, Error> {
        self.conn.write(format!("GET {path} HTTP/1.0\r\n\r\n").as_bytes())?;
        self.flush();
        self.read_to_end()
    }

    fn get(self, path: &str) -> Result<String, Error> {
        Ok(String::from_utf8_lossy(&self.get_bytes(path)?).into_owned())
    }
}

#[test]
fn talks_to_openssl_with_each_key_and_suite() {
    let page = Session::open("rsa", web_pki(), "localhost").unwrap().get("/").unwrap();
    assert!(page.contains("Cipher is TLS_AES_128_GCM_SHA256"), "{page}");
    let page = Session::open("chacha", web_pki(), "localhost").unwrap().get("/").unwrap();
    assert!(page.contains("Cipher is TLS_CHACHA20_POLY1305_SHA256"), "{page}");
    let session = Session::open("ed25519", web_pki(), "localhost").unwrap();
    assert_eq!(session.conn.cipher_suite(), Some(CipherSuite::Aes256GcmSha384));
    assert!(session.get("/").unwrap().contains("TLS_AES_256_GCM_SHA384"));
    Session::open("pss", web_pki(), "localhost").unwrap().get("/").unwrap();
}

#[test]
fn retries_with_the_group_the_server_wants() {
    let page = Session::open("retry", web_pki(), "127.0.0.1").unwrap().get("/").unwrap();
    assert!(page.contains("TLSv1.3"), "{page}");
    Session::open("p384", web_pki(), "localhost").unwrap().get("/").unwrap();
    let mut only_p256 = web_pki();
    only_p256.groups = vec![Group::P256];
    Session::open("retry", only_p256, "localhost").unwrap().get("/").unwrap();
}

#[test]
fn rejects_bad_servers() {
    let name = Session::open("rsa", web_pki(), "example.com").err();
    assert_eq!(name, Some(Error::Certificate(CertError::NameMismatch)));
    let untrusted = Session::open("rsa", ClientConfig::new(Verify::WebPki(TrustStore::new())), "localhost").err();
    assert_eq!(untrusted, Some(Error::Certificate(CertError::UnknownIssuer)));
    let constrained = Session::open("nc", web_pki(), "localhost").err();
    assert_eq!(constrained, Some(Error::Certificate(CertError::NameConstraint)));
    let mut expired = web_pki();
    expired.clock = Some(|| 4_000_000_000);
    assert_eq!(Session::open("chacha", expired, "localhost").err(), Some(Error::Certificate(CertError::Expired)));
    let mut no_common_suite = web_pki();
    no_common_suite.cipher_suites = vec![CipherSuite::Aes128GcmSha256];
    assert_eq!(Session::open("chacha", no_common_suite, "localhost").err(), Some(Error::Alert(40)));
}

#[test]
fn pins_and_skips_verification() {
    let leaf = fixture("leaf_ec.der");
    let spki = Certificate::parse(&leaf).unwrap().spki.to_vec();
    Session::open("chacha", ClientConfig::new(Verify::PinnedSpki(spki)), "anything").unwrap().get("/").unwrap();
    let other = Certificate::parse(&fixture("leaf_rsa.der")).unwrap().spki.to_vec();
    assert!(Session::open("chacha", ClientConfig::new(Verify::PinnedSpki(other)), "localhost").is_err());
    let session = Session::open("rsa", ClientConfig::new(Verify::None), "unknown.example").unwrap();
    assert_eq!(session.conn.peer_certificates().len(), 2);
    session.get("/").unwrap();
}

#[test]
fn authenticates_with_a_client_certificate() {
    let key = ed25519::SigningKey::from_seed(&[9; 32]);
    let cert = CertificateBuilder::new(Name::new().common_name("devkit client")).self_signed(Signer::Ed25519(&key)).unwrap();
    let mut config = web_pki();
    config.identity = Some(Identity { chain: vec![cert], key: PrivateKey::Ed25519(key) });
    let page = Session::open("client_auth", config, "localhost").unwrap().get("/").unwrap();
    assert!(page.contains("CN=devkit client"), "{page}");
    let rsa = crate::rsa::PrivateKey::generate(2048).unwrap();
    let cert = CertificateBuilder::new(Name::new().common_name("devkit rsa client")).self_signed(Signer::Rsa(&rsa)).unwrap();
    let mut config = web_pki();
    config.identity = Some(Identity { chain: vec![cert], key: PrivateKey::Rsa(rsa) });
    let page = Session::open("client_auth", config, "localhost").unwrap().get("/").unwrap();
    assert!(page.contains("CN=devkit rsa client"), "{page}");
    let page = Session::open("client_auth", web_pki(), "localhost").unwrap().get("/").unwrap();
    assert!(page.contains("no client certificate"), "{page}");
}

#[test]
fn moves_bulk_data_both_ways() {
    let big = fixture("big.bin");
    let body = Session::open("files", web_pki(), "localhost").unwrap().get_bytes("/big.bin").unwrap();
    assert!(body.ends_with(&big), "{} bytes", body.len());

    let mut session = Session::open("echo", web_pki(), "localhost").unwrap();
    let mut buf = [0u8; 4096];
    for i in 0..6u8 {
        let line: String = (0..10_000).map(|j| (b'a' + ((i as usize + j) % 26) as u8) as char).collect();
        session.conn.write(format!("{line}\n").as_bytes()).unwrap();
        session.flush();
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !got.ends_with(b"\n") && Instant::now() < deadline {
            let n = session.conn.read(&mut buf).unwrap();
            got.extend_from_slice(&buf[..n]);
            if n == 0 {
                assert!(session.pump().unwrap());
            }
        }
        let reversed: String = line.chars().rev().collect();
        assert_eq!(String::from_utf8_lossy(&got).trim_end(), reversed);
    }
    session.conn.close();
    session.flush();
}

#[test]
fn exports_the_keying_material_the_server_sees() {
    let mut session = Session::open("exporter", web_pki(), "localhost").unwrap();
    session.conn.write(b"export\n").unwrap();
    let reply = String::from_utf8(session.read_to_end().unwrap()).unwrap();
    let mut exported = [0u8; 32];
    session.conn.export_keying_material(b"devkit-test", &[], &mut exported).unwrap();
    let hex: String = exported.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(reply.trim_end(), hex);
}
