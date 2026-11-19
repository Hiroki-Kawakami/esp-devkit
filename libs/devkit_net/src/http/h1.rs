/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;

use super::Headers;
use super::conn::{Conn, Limits};
use crate::{Error, Result};

/// The most a response head may take.
pub(crate) const MAX_HEAD: usize = 32 * 1024;

pub(crate) struct Head {
    pub status: u16,
    pub reason: String,
    /// The y of HTTP/1.y.
    pub minor: u8,
    pub headers: Headers,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Framing {
    Empty,
    Length(u64),
    Chunked,
    /// Until the server closes the connection.
    Close,
}

/// The head and how much of `buf` it took, once `buf` holds all of it.
pub(crate) fn parse_head(buf: &[u8]) -> Result<Option<(Head, usize)>> {
    let start = buf.iter().position(|&b| b != b'\r' && b != b'\n').unwrap_or(buf.len());
    let mut end = None;
    let mut i = start;
    while let Some(off) = buf[i..].iter().position(|&b| b == b'\n') {
        let nl = i + off;
        let next = &buf[nl + 1..];
        if next.first() == Some(&b'\n') {
            end = Some((nl, nl + 2));
            break;
        }
        if next.starts_with(b"\r\n") {
            end = Some((nl, nl + 3));
            break;
        }
        i = nl + 1;
    }
    let Some((last, used)) = end else {
        return if buf.len() > MAX_HEAD { Err(Error::MessageTooLarge) } else { Ok(None) };
    };
    if used > MAX_HEAD {
        return Err(Error::MessageTooLarge);
    }
    let text = String::from_utf8_lossy(&buf[start..last]);
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let status_line = lines.next().ok_or(Error::InvalidData)?;
    let rest = status_line.strip_prefix("HTTP/1.").ok_or(Error::InvalidData)?;
    let minor = rest.bytes().next().filter(u8::is_ascii_digit).ok_or(Error::InvalidData)? - b'0';
    let rest = rest[1..].strip_prefix(' ').ok_or(Error::InvalidData)?;
    let code = rest.get(..3).filter(|c| c.bytes().all(|b| b.is_ascii_digit())).ok_or(Error::InvalidData)?;
    let status: u16 = code.parse().map_err(|_| Error::InvalidData)?;
    let reason = String::from(rest[3..].trim_start());
    let mut headers = Headers::new();
    for line in lines {
        if line.starts_with([' ', '\t']) {
            let value = headers.last_mut().ok_or(Error::InvalidData)?;
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(Error::InvalidData)?;
        if name.is_empty() || name.ends_with([' ', '\t']) {
            return Err(Error::InvalidData);
        }
        headers.append(name, value.trim());
    }
    Ok(Some((Head { status, reason, minor, headers }, used)))
}

/// Reads one head, 1xx ones included.
pub(crate) async fn read_head(conn: &mut Conn, limits: &Limits) -> Result<Head> {
    loop {
        if let Some((head, n)) = parse_head(conn.buffered())? {
            conn.consume(n);
            return Ok(head);
        }
        if conn.fill(limits).await? == 0 {
            return Err(Error::ConnectionAborted);
        }
    }
}

/// Skips interim 1xx responses; 101 ends HTTP on the connection and counts as final.
pub(crate) async fn final_head(conn: &mut Conn, limits: &Limits) -> Result<Head> {
    loop {
        let head = read_head(conn, limits).await?;
        if !is_interim(head.status) {
            return Ok(head);
        }
    }
}

pub(crate) fn is_interim(status: u16) -> bool {
    (100..200).contains(&status) && status != 101
}

fn has_token(headers: &Headers, name: &str, token: &str) -> bool {
    headers.get_all(name).flat_map(|v| v.split(',')).any(|t| t.trim().eq_ignore_ascii_case(token))
}

pub(crate) fn framing(head: &Head, head_request: bool) -> Result<Framing> {
    if head_request || head.status < 200 || head.status == 204 || head.status == 304 {
        return Ok(Framing::Empty);
    }
    if let Some(last) = head.headers.get_all("transfer-encoding").flat_map(|v| v.split(',')).last() {
        return Ok(if last.trim().eq_ignore_ascii_case("chunked") { Framing::Chunked } else { Framing::Close });
    }
    match content_length(&head.headers)? {
        Some(n) => Ok(Framing::Length(n)),
        None => Ok(Framing::Close),
    }
}

/// Copies that agree count as one.
pub(crate) fn content_length(headers: &Headers) -> Result<Option<u64>> {
    let mut found = None;
    for value in headers.get_all("content-length").flat_map(|v| v.split(',')) {
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::InvalidData);
        }
        let n: u64 = value.parse().map_err(|_| Error::InvalidData)?;
        if found.is_some_and(|f| f != n) {
            return Err(Error::InvalidData);
        }
        found = Some(n);
    }
    Ok(found)
}

/// Whether the connection may carry another request after this response.
pub(crate) fn keep_alive(head: &Head) -> bool {
    if head.status == 101 || has_token(&head.headers, "connection", "close") {
        return false;
    }
    head.minor >= 1 || has_token(&head.headers, "connection", "keep-alive")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Head {
        let (head, used) = parse_head(s.as_bytes()).unwrap().unwrap();
        assert_eq!(used, s.len());
        head
    }

    #[test]
    fn heads_parse_once_complete() {
        let full = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Folded: a\r\n  b\r\n\r\n";
        for cut in 0..full.len() {
            assert!(parse_head(&full.as_bytes()[..cut]).unwrap().is_none(), "cut at {cut}");
        }
        let head = parse(full);
        assert_eq!((head.status, head.reason.as_str(), head.minor), (200, "OK", 1));
        assert_eq!(head.headers.get("content-length"), Some("5"));
        assert_eq!(head.headers.get("x-folded"), Some("a b"));
        let bare = parse("\r\nHTTP/1.0 404\nServer: x\n\n");
        assert_eq!((bare.status, bare.reason.as_str(), bare.minor), (404, "", 0));
        assert!(parse_head(b"HTTP/2 200 OK\r\n\r\n").is_err());
        assert!(parse_head(b"HTTP/1.1 20 OK\r\n\r\n").is_err());
        assert!(parse_head(b"HTTP/1.1 200 OK\r\nBad : x\r\n\r\n").is_err());
        let huge = [b'a'; MAX_HEAD + 1];
        assert_eq!(parse_head(&huge).err(), Some(Error::MessageTooLarge));
    }

    #[test]
    fn framing_follows_the_head() {
        let f = |s: &str, head_request: bool| framing(&parse(s), head_request);
        assert_eq!(f("HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n", false), Ok(Framing::Length(7)));
        assert_eq!(f("HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n", true), Ok(Framing::Empty));
        assert_eq!(f("HTTP/1.1 204 No Content\r\n\r\n", false), Ok(Framing::Empty));
        assert_eq!(f("HTTP/1.1 304 Not Modified\r\nContent-Length: 9\r\n\r\n", false), Ok(Framing::Empty));
        assert_eq!(f("HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\nContent-Length: 3\r\n\r\n", false), Ok(Framing::Chunked));
        assert_eq!(f("HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n", false), Ok(Framing::Close));
        assert_eq!(f("HTTP/1.1 200 OK\r\n\r\n", false), Ok(Framing::Close));
        assert_eq!(f("HTTP/1.1 200 OK\r\nContent-Length: 3, 3\r\n\r\n", false), Ok(Framing::Length(3)));
        assert_eq!(f("HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 4\r\n\r\n", false), Err(Error::InvalidData));
        assert_eq!(f("HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n", false), Err(Error::InvalidData));
    }

    #[test]
    fn keep_alive_follows_version_and_connection() {
        assert!(keep_alive(&parse("HTTP/1.1 200 OK\r\n\r\n")));
        assert!(!keep_alive(&parse("HTTP/1.1 200 OK\r\nConnection: Close\r\n\r\n")));
        assert!(!keep_alive(&parse("HTTP/1.0 200 OK\r\n\r\n")));
        assert!(keep_alive(&parse("HTTP/1.0 200 OK\r\nConnection: keep-alive\r\n\r\n")));
    }
}
