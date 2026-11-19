/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::{Error, Result};

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Origin {
    pub tls: bool,
    /// Lowercase, without the brackets of an IPv6 literal.
    pub host: String,
    pub port: u16,
}

impl Origin {
    fn default_port(&self) -> u16 {
        if self.tls { 443 } else { 80 }
    }

    /// As the Host header carries it.
    pub fn authority(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == self.default_port() { host } else { format!("{host}:{}", self.port) }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Url {
    pub origin: Origin,
    /// Path and query, from the first '/', percent-encoded.
    pub target: String,
    pub user: Option<String>,
    pub password: Option<String>,
}

impl Url {
    pub fn parse(s: &str) -> Result<Self> {
        let (scheme, rest) = s.trim().split_once("://").ok_or(Error::InvalidInput)?;
        let tls = if scheme.eq_ignore_ascii_case("https") {
            true
        } else if scheme.eq_ignore_ascii_case("http") {
            false
        } else {
            return Err(Error::InvalidInput);
        };
        let rest = rest.split('#').next().unwrap_or("");
        let (authority, target) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (host, port) = split_host_port(hostport)?;
        let origin = Origin { tls, host, port: 0 };
        let port = port.unwrap_or(origin.default_port());
        let (user, password) = match userinfo {
            None => (None, None),
            Some(info) => match info.split_once(':') {
                Some((user, password)) => (Some(decode(user)?), Some(decode(password)?)),
                None => (Some(decode(info)?), None),
            },
        };
        Ok(Self { origin: Origin { port, ..origin }, target: encode_target(target), user, password })
    }

    /// Where `location` points from here, as a redirect gives it.
    pub fn join(&self, location: &str) -> Result<Url> {
        let location = location.trim();
        if let Some(i) = location.find("://") {
            if location[..i].bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b)) {
                return Url::parse(location);
            }
        }
        let scheme = if self.origin.tls { "https" } else { "http" };
        if let Some(rest) = location.strip_prefix("//") {
            return Url::parse(&format!("{scheme}://{rest}"));
        }
        let location = location.split('#').next().unwrap_or("");
        let path = self.target.split('?').next().unwrap_or("/");
        let target = if location.starts_with('/') {
            location.to_string()
        } else if location.starts_with('?') {
            format!("{path}{location}")
        } else if location.is_empty() {
            self.target.clone()
        } else {
            format!("{}{location}", &path[..path.rfind('/').map_or(0, |i| i + 1)])
        };
        let mut url = self.clone();
        url.target = encode_target(&remove_dots(&target));
        Ok(url)
    }

    /// Without the user and password.
    pub fn href(&self) -> String {
        let scheme = if self.origin.tls { "https" } else { "http" };
        format!("{scheme}://{}{}", self.origin.authority(), self.target)
    }
}

fn split_host_port(s: &str) -> Result<(String, Option<u16>)> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or(Error::InvalidInput)?;
        match after {
            "" => (host, None),
            _ => (host, Some(after.strip_prefix(':').ok_or(Error::InvalidInput)?)),
        }
    } else {
        match s.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (s, None),
        }
    };
    if host.is_empty() {
        return Err(Error::InvalidInput);
    }
    let port = match port {
        None | Some("") => None,
        Some(p) => Some(p.parse::<u16>().ok().filter(|&p| p != 0).ok_or(Error::InvalidInput)?),
    };
    Ok((host.to_ascii_lowercase(), port))
}

fn decode(s: &str) -> Result<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3).ok_or(Error::InvalidInput)?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| Error::InvalidInput)?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| Error::InvalidInput)
}

/// From '/', with what a request line cannot carry percent-encoded.
fn encode_target(target: &str) -> String {
    let mut out = String::with_capacity(target.len() + 1);
    if !target.starts_with('/') {
        out.push('/');
    }
    for &b in target.as_bytes() {
        if b <= b' ' || b >= 0x7F {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

fn remove_dots(target: &str) -> String {
    let (path, query) = target.split_at(target.find('?').unwrap_or(target.len()));
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let mut out: Vec<&str> = Vec::new();
    for (i, segment) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *segment {
            "." => {}
            ".." => {
                out.pop();
            }
            s => {
                out.push(s);
                continue;
            }
        }
        if last {
            out.push("");
        }
    }
    let mut s = String::new();
    for segment in &out {
        s.push('/');
        s.push_str(segment);
    }
    if s.is_empty() {
        s.push('/');
    }
    s + query
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse() {
        let url = Url::parse("HTTPS://us%40er:p%3Ass@Example.COM:8443/a b?q=1#frag").unwrap();
        assert_eq!(url.origin, Origin { tls: true, host: String::from("example.com"), port: 8443 });
        assert_eq!((url.user.as_deref(), url.password.as_deref()), (Some("us@er"), Some("p:ss")));
        assert_eq!(url.target, "/a%20b?q=1");
        assert_eq!(url.href(), "https://example.com:8443/a%20b?q=1");
        let bare = Url::parse("http://host").unwrap();
        assert_eq!((bare.origin.port, bare.target.as_str(), bare.origin.authority()), (80, "/", String::from("host")));
        assert_eq!(Url::parse("http://h?x").unwrap().target, "/?x");
        let v6 = Url::parse("http://[::1]:8080/").unwrap();
        assert_eq!((v6.origin.host.as_str(), v6.origin.authority()), ("::1", String::from("[::1]:8080")));
        for bad in ["ftp://h/", "h/x", "http:///x", "http://h:99999/", "http://h:0/", "http://[::1/", "http://u%zz@h/"] {
            assert_eq!(Url::parse(bad).err(), Some(Error::InvalidInput), "{bad}");
        }
    }

    #[test]
    fn locations_resolve() {
        let base = Url::parse("http://h:81/a/b/x?y").unwrap();
        let join = |loc: &str| base.join(loc).unwrap().href();
        assert_eq!(join("https://o/p"), "https://o/p");
        assert_eq!(join("//o/p"), "http://o/p");
        assert_eq!(join("/p?q"), "http://h:81/p?q");
        assert_eq!(join("?z"), "http://h:81/a/b/x?z");
        assert_eq!(join("c"), "http://h:81/a/b/c");
        assert_eq!(join("../c"), "http://h:81/a/c");
        assert_eq!(join("../../../c"), "http://h:81/c");
        assert_eq!(join("."), "http://h:81/a/b/");
        assert_eq!(join(".."), "http://h:81/a/");
        assert_eq!(join("./c/./d"), "http://h:81/a/b/c/d");
        assert_eq!(join(""), "http://h:81/a/b/x?y");
        assert_eq!(join("c#f"), "http://h:81/a/b/c");
    }
}
