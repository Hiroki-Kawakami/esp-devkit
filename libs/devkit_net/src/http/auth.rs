/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use devkit_crypto::hash::{Hash, Md5, Sha256};

use super::Headers;

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { BASE64[(n >> (18 - 6 * i)) as usize & 63] as char } else { '=' });
        }
    }
    out
}

pub(crate) fn basic(user: &str, password: &str) -> String {
    format!("Basic {}", base64(format!("{user}:{password}").as_bytes()))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Alg {
    Md5,
    Sha256,
}

/// A Digest challenge this client can answer.
#[derive(Clone, Debug)]
pub(crate) struct Digest {
    alg: Alg,
    sess: bool,
    /// As the server named it, to send back.
    alg_name: String,
    realm: String,
    nonce: String,
    opaque: Option<String>,
    qop: bool,
}

impl Digest {
    /// The strongest Digest challenge among the WWW-Authenticate fields.
    pub fn from_headers(headers: &Headers) -> Option<Self> {
        let mut best: Option<Digest> = None;
        for value in headers.get_all("www-authenticate") {
            for (scheme, params) in challenges(value) {
                if !scheme.eq_ignore_ascii_case("digest") {
                    continue;
                }
                let Some(digest) = Self::from_params(&params) else { continue };
                if best.as_ref().is_none_or(|b| b.alg == Alg::Md5 && digest.alg == Alg::Sha256) {
                    best = Some(digest);
                }
            }
        }
        best
    }

    fn from_params(params: &[(String, String)]) -> Option<Self> {
        let get = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        let alg_name = get("algorithm").unwrap_or_else(|| String::from("MD5"));
        let (base, sess) = match alg_name.len().checked_sub(5) {
            Some(i) if alg_name[i..].eq_ignore_ascii_case("-sess") => (&alg_name[..i], true),
            _ => (alg_name.as_str(), false),
        };
        let alg = if base.eq_ignore_ascii_case("md5") {
            Alg::Md5
        } else if base.eq_ignore_ascii_case("sha-256") {
            Alg::Sha256
        } else {
            return None;
        };
        let qop = match get("qop") {
            None => false,
            Some(list) if list.split(',').any(|q| q.trim().eq_ignore_ascii_case("auth")) => true,
            Some(_) => return None,
        };
        Some(Self { alg, sess, alg_name, realm: get("realm")?, nonce: get("nonce")?, opaque: get("opaque"), qop })
    }

    pub fn authorization(&self, user: &str, password: &str, method: &str, uri: &str) -> String {
        let mut raw = [0u8; 8];
        devkit_crypto::rand::fill(&mut raw);
        let cnonce: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        self.answer(user, password, method, uri, &cnonce)
    }

    fn answer(&self, user: &str, password: &str, method: &str, uri: &str, cnonce: &str) -> String {
        let h = |parts: &[&str]| match self.alg {
            Alg::Md5 => hex::<Md5>(parts),
            Alg::Sha256 => hex::<Sha256>(parts),
        };
        let mut ha1 = h(&[user, &self.realm, password]);
        if self.sess {
            ha1 = h(&[&ha1, &self.nonce, cnonce]);
        }
        let ha2 = h(&[method, uri]);
        let nc = "00000001";
        let response =
            if self.qop { h(&[&ha1, &self.nonce, nc, cnonce, "auth", &ha2]) } else { h(&[&ha1, &self.nonce, &ha2]) };
        let mut out = format!(
            "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", algorithm={}, response=\"{response}\"",
            quote(user),
            quote(&self.realm),
            quote(&self.nonce),
            quote(uri),
            self.alg_name
        );
        if let Some(opaque) = &self.opaque {
            out.push_str(&format!(", opaque=\"{}\"", quote(opaque)));
        }
        if self.qop {
            out.push_str(&format!(", qop=auth, nc={nc}, cnonce=\"{cnonce}\""));
        }
        out
    }
}

fn hex<H: Hash>(parts: &[&str]) -> String {
    let mut hash = H::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hash.update(b":");
        }
        hash.update(part.as_bytes());
    }
    hash.finalize().as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Each challenge's scheme and parameters, names lowercased; a token68 ends
/// up as a parameter without a value.
fn challenges(value: &str) -> Vec<(String, Vec<(String, String)>)> {
    let b = value.as_bytes();
    let skip = |i: &mut usize, set: &[u8]| {
        while *i < b.len() && set.contains(&b[*i]) {
            *i += 1;
        }
    };
    let token = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && is_tchar(b[*i]) {
            *i += 1;
        }
        &value[start..*i]
    };
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        skip(&mut i, b" \t,");
        let scheme = token(&mut i);
        if scheme.is_empty() {
            return out;
        }
        let mut params = Vec::new();
        loop {
            let save = i;
            skip(&mut i, b" \t,");
            let key = token(&mut i);
            let mut j = i;
            skip(&mut j, b" \t");
            if key.is_empty() || b.get(j) != Some(&b'=') {
                i = save;
                break;
            }
            i = j + 1;
            skip(&mut i, b" \t");
            let mut val = String::new();
            if b.get(i) == Some(&b'"') {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                    }
                    let ch = value[i..].chars().next().unwrap_or('\0');
                    val.push(ch);
                    i += ch.len_utf8();
                }
                i += 1;
            } else {
                val.push_str(token(&mut i));
            }
            params.push((key.to_ascii_lowercase(), val));
        }
        out.push((String::from(scheme), params));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(basic("Aladdin", "open sesame"), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }

    #[test]
    fn challenges_split_and_unquote() {
        let got = challenges(r#"Basic realm="a, b", Digest realm="x\"y", nonce=abc, qop="auth,auth-int""#);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "Basic");
        assert_eq!(got[0].1, [(String::from("realm"), String::from("a, b"))]);
        assert_eq!(got[1].0, "Digest");
        assert_eq!(got[1].1[0].1, "x\"y");
        assert_eq!(got[1].1[1], (String::from("nonce"), String::from("abc")));
        assert_eq!(got[1].1[2].1, "auth,auth-int");
    }

    fn rfc7616(alg: &str) -> String {
        let mut headers = Headers::new();
        headers.append(
            "WWW-Authenticate",
            &format!(
                "Digest realm=\"http-auth@example.org\", qop=\"auth, auth-int\", algorithm={alg}, \
                 nonce=\"7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v\", \
                 opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\""
            ),
        );
        let digest = Digest::from_headers(&headers).unwrap();
        let cnonce = "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ";
        digest.answer("Mufasa", "Circle of Life", "GET", "/dir/index.html", cnonce)
    }

    #[test]
    fn digest_answers_md5_and_sha256() {
        let md5 = rfc7616("MD5");
        assert!(md5.contains("response=\"8ca523f5e9506fed4657c9700eebdbec\""), "{md5}");
        assert!(md5.contains("opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\""));
        assert!(md5.contains("qop=auth, nc=00000001, cnonce=\"f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ\""));
        let sha = rfc7616("SHA-256");
        assert!(sha.contains("response=\"753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1\""), "{sha}");
        assert!(sha.contains("algorithm=SHA-256"));
    }

    #[test]
    fn digest_prefers_sha256_and_skips_what_it_cannot_answer() {
        let mut headers = Headers::new();
        headers.append("WWW-Authenticate", "Digest realm=r, nonce=n, algorithm=MD5");
        headers.append("WWW-Authenticate", "Digest realm=r, nonce=n, algorithm=SHA-256");
        assert_eq!(Digest::from_headers(&headers).unwrap().alg, Alg::Sha256);
        let mut only = Headers::new();
        only.append("WWW-Authenticate", "Digest realm=r, nonce=n, qop=auth-int");
        only.append("WWW-Authenticate", "Digest realm=r, nonce=n, algorithm=SHA-512-256");
        assert!(Digest::from_headers(&only).is_none());
    }
}
