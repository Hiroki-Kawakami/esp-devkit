/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::collections::VecDeque;
use std::ffi::CString;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use super::in_task;

use crate::Error;
use crate::http::{Client, ClientBuilder, Connector, Response, Started, TestTransport, Version};

enum Gate {
    /// Once this many request lines went out.
    Requests(usize),
    /// Once the bytes sent contain this.
    Seen(&'static [u8]),
}

type Responder = Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>;

/// One connection of the scripted server: replies released as the client's
/// requests come in, or made by a responder from each write.
#[derive(Default)]
pub(crate) struct Wire {
    replies: VecDeque<(Gate, Vec<u8>)>,
    out: VecDeque<u8>,
    chunk: usize,
    sent: Vec<u8>,
    /// EOF once every reply was read.
    close: bool,
    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    h2: bool,
    responder: Option<Responder>,
    waker: Option<Waker>,
}

impl Wire {
    /// A connection whose ALPN chose h2, answered by `respond`.
    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    pub(crate) fn h2(respond: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static) -> Self {
        Self { chunk: usize::MAX, h2: true, responder: Some(Box::new(respond)), ..Self::default() }
    }

    fn new(replies: &[&str]) -> Self {
        let replies = replies.iter().enumerate().map(|(i, r)| (Gate::Requests(i + 1), r.as_bytes().to_vec())).collect();
        Self { replies, chunk: usize::MAX, ..Self::default() }
    }

    fn gated(replies: Vec<(Gate, Vec<u8>)>) -> Self {
        Self { replies: replies.into(), chunk: usize::MAX, ..Self::default() }
    }

    fn chunk(mut self, n: usize) -> Self {
        self.chunk = n;
        self
    }

    fn closing(mut self) -> Self {
        self.close = true;
        self
    }

    fn release(&mut self) {
        while let Some((gate, _)) = self.replies.front() {
            let open = match gate {
                Gate::Requests(n) => self.sent.windows(11).filter(|w| w == b" HTTP/1.1\r\n").count() >= *n,
                Gate::Seen(pattern) => self.sent.windows(pattern.len()).any(|w| w == *pattern),
            };
            if !open {
                return;
            }
            let (_, reply) = self.replies.pop_front().unwrap();
            self.out.extend(reply);
        }
    }
}

#[derive(Clone)]
pub(crate) struct Mock(Arc<Mutex<Wire>>);

impl Mock {
    #[cfg(CONFIG_DEVKIT_NET_HTTP2)]
    pub(crate) fn h2(&self) -> bool {
        self.0.lock().unwrap().h2
    }

    /// Waits until the script releases a reply, which only a write does.
    pub(crate) async fn read(&mut self, buf: &mut [u8]) -> crate::Result<usize> {
        core::future::poll_fn(|cx| {
            let mut wire = self.0.lock().unwrap();
            wire.release();
            if wire.out.is_empty() {
                if wire.close && wire.replies.is_empty() {
                    return Poll::Ready(Ok(0));
                }
                wire.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            let n = buf.len().min(wire.chunk).min(wire.out.len());
            for (dst, src) in buf.iter_mut().zip(wire.out.drain(..n)) {
                *dst = src;
            }
            Poll::Ready(Ok(n))
        })
        .await
    }

    pub(crate) fn write_all(&mut self, buf: &[u8]) -> crate::Result<()> {
        let mut wire = self.0.lock().unwrap();
        wire.sent.extend_from_slice(buf);
        if let Some(mut respond) = wire.responder.take() {
            let reply = respond(buf);
            wire.out.extend(reply);
            wire.responder = Some(respond);
        }
        if let Some(waker) = wire.waker.take() {
            waker.wake();
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Server {
    waiting: Mutex<VecDeque<Arc<Mutex<Wire>>>>,
    opened: Mutex<Vec<(String, Arc<Mutex<Wire>>)>>,
}

impl Server {
    pub(crate) fn opened(&self) -> usize {
        self.opened.lock().unwrap().len()
    }

    fn host(&self, i: usize) -> String {
        self.opened.lock().unwrap()[i].0.clone()
    }

    fn sent(&self, i: usize) -> String {
        String::from_utf8_lossy(&self.opened.lock().unwrap()[i].1.lock().unwrap().sent).into_owned()
    }
}

pub(crate) fn serve(builder: ClientBuilder, wires: Vec<Wire>) -> (Client, Arc<Server>) {
    let server = Arc::new(Server::default());
    server.waiting.lock().unwrap().extend(wires.into_iter().map(|w| Arc::new(Mutex::new(w))));
    let s = server.clone();
    let connector: Connector = Arc::new(move |host: &str| {
        let wire = s.waiting.lock().unwrap().pop_front().ok_or(Error::ConnectionRefused)?;
        s.opened.lock().unwrap().push((String::from(host), wire.clone()));
        Ok(TestTransport::Mock(Box::new(Mock(wire))))
    });
    (builder.connector(connector).build(), server)
}

pub(crate) fn read_all(resp: &mut Response) -> crate::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 100];
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

const NO_CONTENT: &str = "HTTP/1.1 204 No Content\r\n\r\n";

#[test]
fn a_length_body_and_the_request_head() {
    in_task(|| {
        let (client, server) = serve(Client::builder(), vec![Wire::new(&["HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"])]);
        let mut resp = client.get("http://example.com:8080/a b?q=1").header("X-Test", "1").send().unwrap();
        assert_eq!((resp.status(), resp.reason(), resp.content_length()), (200, "OK", Some(5)));
        assert_eq!(resp.version(), Version::Http11);
        assert_eq!(resp.url(), "http://example.com:8080/a%20b?q=1");
        assert_eq!(read_all(&mut resp).unwrap(), b"hello");
        assert_eq!(resp.received(), 5);
        let sent = server.sent(0);
        assert!(sent.starts_with("GET /a%20b?q=1 HTTP/1.1\r\nHost: example.com:8080\r\nUser-Agent: devkit_net\r\nAccept: */*\r\n"), "{sent}");
        assert!(sent.ends_with("X-Test: 1\r\n\r\n"), "{sent}");
        assert_eq!(sent.contains("Accept-Encoding: gzip, deflate\r\n"), cfg!(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS));
    });
}

#[test]
fn a_chunked_body_in_single_bytes_and_a_kept_connection() {
    in_task(|| {
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n6\r\n world\r\n0\r\nX-Trailer: t\r\n\r\n";
        let (client, server) = serve(Client::builder(), vec![Wire::new(&[chunked, NO_CONTENT]).chunk(1)]);
        let mut resp = client.get("http://h/").send().unwrap();
        assert_eq!(resp.content_length(), None);
        assert_eq!(read_all(&mut resp).unwrap(), b"hello world");
        assert_eq!(resp.received(), 11);
        assert_eq!(client.get("http://h/2").send().unwrap().status(), 204);
        assert_eq!(server.opened(), 1);
    });
}

#[test]
fn a_kept_connection_the_server_closed_gets_the_request_again() {
    in_task(|| {
        let first = Wire::new(&["HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\na"]).closing();
        let (client, server) = serve(Client::builder(), vec![first, Wire::new(&[NO_CONTENT])]);
        assert_eq!(read_all(&mut client.get("http://h/").send().unwrap()).unwrap(), b"a");
        assert_eq!(client.get("http://h/").send().unwrap().status(), 204);
        assert_eq!(server.opened(), 2);
    });
}

#[test]
fn connections_that_cannot_go_on_are_not_kept() {
    in_task(|| {
        let wires = vec![
            Wire::new(&["HTTP/1.1 200 OK\r\n\r\nbye"]).closing(),
            Wire::new(&["HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nleft"]),
            Wire::new(&["HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"]),
            Wire::new(&[NO_CONTENT]),
        ];
        let (client, server) = serve(Client::builder(), wires);
        assert_eq!(read_all(&mut client.get("http://h/").send().unwrap()).unwrap(), b"bye");
        drop(client.get("http://h/").send().unwrap());
        client.get("http://h/").send().unwrap();
        assert_eq!(client.get("http://h/").send().unwrap().status(), 204);
        assert_eq!(server.opened(), 4);
    });
}

#[test]
fn redirects_keep_or_change_the_method() {
    in_task(|| {
        let ok = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let wire = Wire::new(&["HTTP/1.1 302 Found\r\nLocation: next\r\nContent-Length: 2\r\n\r\nno", ok]);
        let (client, server) = serve(Client::builder(), vec![wire]);
        let mut resp = client.get("http://h/a/b").send().unwrap();
        assert_eq!((resp.status(), resp.url()), (200, "http://h/a/next"));
        assert_eq!(read_all(&mut resp).unwrap(), b"ok");
        assert_eq!(server.opened(), 1);
        assert!(server.sent(0).contains("GET /a/next HTTP/1.1\r\n"));

        let see_other = "HTTP/1.1 303 See Other\r\nLocation: /x\r\nContent-Length: 0\r\n\r\n";
        let temporary = "HTTP/1.1 307 Temporary Redirect\r\nLocation: /y\r\nContent-Length: 0\r\n\r\n";
        let (client, server) = serve(Client::builder(), vec![Wire::new(&[see_other, ok, temporary, ok])]);
        assert_eq!(client.post("http://h/").body(b"data").send().unwrap().bytes(2).unwrap(), b"ok");
        let sent = server.sent(0);
        let get = &sent[sent.find("GET /x").unwrap()..];
        assert!(!get.contains("Content-Length") && !get.contains("data"), "{sent}");
        assert_eq!(client.post("http://h/").body(b"data").send().unwrap().bytes(2).unwrap(), b"ok");
        assert_eq!(server.opened(), 1);
        let sent = server.sent(0);
        assert!(sent[sent.find("POST /y").unwrap()..].contains("Content-Length: 4\r\n"), "{sent}");
        assert!(sent.ends_with("\r\n\r\ndata"), "{sent}");
    });
}

#[test]
fn credentials_stay_on_their_origin() {
    in_task(|| {
        let moved = "HTTP/1.1 301 Moved\r\nLocation: http://b.example/\r\nContent-Length: 0\r\n\r\n";
        let (client, server) = serve(Client::builder(), vec![Wire::new(&[moved]), Wire::new(&[NO_CONTENT])]);
        let resp = client.get("http://u:p@a.example/").header("Cookie", "c=1").send().unwrap();
        assert_eq!((resp.status(), resp.url()), (204, "http://b.example/"));
        assert_eq!((server.host(0), server.host(1)), (String::from("a.example"), String::from("b.example")));
        assert!(server.sent(0).contains("Authorization: Basic dTpw\r\n"));
        assert!(!server.sent(1).contains("Authorization") && !server.sent(1).contains("Cookie"));
    });
}

#[test]
fn redirects_stop_at_the_limit() {
    in_task(|| {
        let again = "HTTP/1.1 302 Found\r\nLocation: /r\r\nContent-Length: 0\r\n\r\n";
        let (client, _) = serve(Client::builder().redirects(2), vec![Wire::new(&[again, again, again])]);
        assert_eq!(client.get("http://h/").send().err(), Some(Error::TooManyRedirects));
        let (client, _) = serve(Client::builder().redirects(0), vec![Wire::new(&[again])]);
        assert_eq!(client.get("http://h/").send().unwrap().status(), 302);
    });
}

#[test]
fn digest_answers_one_challenge() {
    in_task(|| {
        let challenge = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"r\"\r\n\
                         WWW-Authenticate: Digest realm=\"r\", nonce=\"n\", qop=\"auth\"\r\nContent-Length: 0\r\n\r\n";
        let (client, server) = serve(Client::builder(), vec![Wire::new(&[challenge, NO_CONTENT, challenge, challenge])]);
        assert_eq!(client.get("http://h/p").credentials("u", "p").send().unwrap().status(), 204);
        let sent = server.sent(0);
        assert!(sent.contains("Authorization: Basic dTpw\r\n"));
        assert!(sent.contains("Authorization: Digest username=\"u\", realm=\"r\", nonce=\"n\", uri=\"/p\", algorithm=MD5"), "{sent}");
        assert_eq!(client.get("http://h/p").credentials("u", "p").send().unwrap().status(), 401);
        assert_eq!(server.opened(), 1);
    });
}

#[test]
fn a_streamed_body_waits_for_continue() {
    in_task(|| {
        let replies = vec![
            (Gate::Requests(1), b"HTTP/1.1 100 Continue\r\n\r\n".to_vec()),
            (Gate::Seen(b"0\r\n\r\n"), b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n".to_vec()),
        ];
        let (client, server) = serve(Client::builder(), vec![Wire::gated(replies)]);
        let Started::Body(mut body) = client.put("http://h/up").start().unwrap() else { panic!("no body asked") };
        body.write_all(b"hello").unwrap();
        body.write_all(b"").unwrap();
        body.write_all(b" world").unwrap();
        assert_eq!(body.finish().unwrap().status(), 201);
        let sent = server.sent(0);
        assert!(sent.contains("Transfer-Encoding: chunked\r\nExpect: 100-continue\r\n"), "{sent}");
        assert!(sent.ends_with("\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"), "{sent}");
    });
}

#[test]
fn a_streamed_body_the_server_turns_down_is_not_sent() {
    in_task(|| {
        let refused = "HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\n\r\n";
        let (client, server) = serve(Client::builder(), vec![Wire::new(&[refused]), Wire::new(&[NO_CONTENT])]);
        let Started::Response(resp) = client.post("http://h/").start().unwrap() else { panic!("body asked") };
        assert_eq!(resp.status(), 413);
        assert_eq!(client.get("http://h/").send().unwrap().status(), 204);
        assert_eq!(server.opened(), 2);
    });
}

#[test]
fn a_silent_server_gets_the_body_after_a_second() {
    in_task(|| {
        let done = (Gate::Seen(b"0\r\n\r\n"), NO_CONTENT.as_bytes().to_vec());
        let (client, _) = serve(Client::builder(), vec![Wire::gated(vec![done])]);
        let start = Instant::now();
        let Started::Body(mut body) = client.post("http://h/").start().unwrap() else { panic!("no body asked") };
        assert!(start.elapsed() >= Duration::from_millis(900), "{:?}", start.elapsed());
        body.write_all(b"x").unwrap();
        assert_eq!(body.finish().unwrap().status(), 204);
    });
}

#[test]
fn a_streamed_body_of_known_length_keeps_to_it() {
    in_task(|| {
        let wires = vec![Wire::new(&[NO_CONTENT]), Wire::new(&[NO_CONTENT])];
        let (client, server) = serve(Client::builder(), wires);
        let Started::Body(mut body) = client.put("http://h/").content_length(3).start().unwrap() else { panic!() };
        assert_eq!(body.write_all(b"four"), Err(Error::InvalidInput));
        body.write_all(b"ab").unwrap();
        assert_eq!(body.finish().err(), Some(Error::InvalidInput));
        assert!(!server.sent(0).contains("Expect"));
        let Started::Body(mut body) = client.put("http://h/").content_length(3).start().unwrap() else { panic!() };
        body.write_all(b"abc").unwrap();
        assert_eq!(body.finish().unwrap().status(), 204);
        assert!(server.sent(1).contains("Content-Length: 3\r\n"));
    });
}

#[test]
fn bytes_keeps_to_its_limit() {
    in_task(|| {
        let held = vec![(Gate::Requests(1), b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n".to_vec())];
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nb\r\nhello world\r\n0\r\n\r\n";
        let head = "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n";
        let wires = vec![Wire::gated(held), Wire::new(&[chunked]), Wire::new(&[chunked, head, NO_CONTENT])];
        let (client, server) = serve(Client::builder().idle_timeout(Some(Duration::from_millis(200))), wires);
        assert_eq!(client.get("http://h/").send().unwrap().bytes(10).err(), Some(Error::MessageTooLarge));
        assert_eq!(client.get("http://h/").send().unwrap().bytes(5).err(), Some(Error::MessageTooLarge));
        assert_eq!(client.get("http://h/").send().unwrap().bytes(11).unwrap(), b"hello world");
        let mut head = client.head("http://h/").send().unwrap();
        assert_eq!(head.content_length(), Some(1000));
        assert_eq!(head.bytes(10).unwrap(), b"");
        assert_eq!(client.get("http://h/").send().unwrap().status(), 204);
        assert_eq!(server.opened(), 3);
    });
}

#[test]
fn save_writes_the_body_to_a_file() {
    in_task(|| {
        let (client, _) = serve(Client::builder(), vec![Wire::new(&["HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsaved!"]).chunk(2)]);
        let path = std::env::temp_dir().join(format!("devkit_net_http_{}.bin", std::process::id()));
        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(client.get("http://h/").send().unwrap().save(&c_path).unwrap(), 6);
        assert_eq!(std::fs::read(&path).unwrap(), b"saved!");
        std::fs::remove_file(&path).unwrap();
    });
}

#[test]
fn timeouts_end_the_waits() {
    in_task(|| {
        let never = vec![(Gate::Seen(b"never"), Vec::new())];
        let (client, _) = serve(Client::builder().idle_timeout(Some(Duration::from_millis(100))), vec![Wire::gated(never)]);
        assert_eq!(client.get("http://h/").send().err(), Some(Error::TimedOut));

        let head = vec![(Gate::Requests(1), b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n".to_vec())];
        let builder = Client::builder().idle_timeout(None).timeout(Some(Duration::from_millis(150)));
        let (client, _) = serve(builder, vec![Wire::gated(head)]);
        let mut resp = client.get("http://h/").send().unwrap();
        let start = Instant::now();
        assert_eq!(resp.read(&mut [0; 9]).err(), Some(Error::TimedOut));
        assert!(start.elapsed() < Duration::from_secs(1));
    });
}

#[cfg(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS)]
mod decompress {
    use super::*;

    const RAW: [u8; 38] = [
        0xed, 0xc6, 0xb1, 0x09, 0x00, 0x20, 0x0c, 0x00, 0xb0, 0x57, 0xfc, 0xac, 0x4b, 0x3b, 0x14, 0xc1, 0xa9, 0x78, 0xbf,
        0x77, 0x08, 0xc9, 0x94, 0xac, 0xbb, 0x7b, 0xe2, 0xd4, 0xac, 0x54, 0x55, 0x55, 0x55, 0x55, 0x55, 0xfd, 0xbd, 0x0f,
    ];

    fn text() -> Vec<u8> {
        b"devkit_net ".repeat(300)
    }

    fn gzip() -> Vec<u8> {
        [&[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff][..], &RAW, &[0xec, 0xa2, 0xc2, 0xc3, 0xe4, 0x0c, 0x00, 0x00]]
            .concat()
    }

    fn zlib() -> Vec<u8> {
        [&[0x78, 0xda][..], &RAW, &[0xf8, 0xdd, 0x0a, 0x88]].concat()
    }

    fn length_reply(encoding: &str, body: &[u8]) -> Vec<u8> {
        let head = format!("HTTP/1.1 200 OK\r\nContent-Encoding: {encoding}\r\nContent-Length: {}\r\n\r\n", body.len());
        [head.as_bytes(), body].concat()
    }

    fn chunked_reply(encoding: &str, body: &[u8]) -> Vec<u8> {
        let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Encoding: {encoding}\r\nTransfer-Encoding: chunked\r\n\r\n").into_bytes();
        for piece in body.chunks(5) {
            reply.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
            reply.extend_from_slice(piece);
            reply.extend_from_slice(b"\r\n");
        }
        reply.extend_from_slice(b"0\r\n\r\n");
        reply
    }

    fn wire(replies: Vec<Vec<u8>>, chunk: usize) -> Wire {
        Wire::gated(replies.into_iter().enumerate().map(|(i, r)| (Gate::Requests(i + 1), r)).collect()).chunk(chunk)
    }

    #[test]
    fn bodies_come_decoded() {
        in_task(|| {
            for chunk in [1, 7, 4096] {
                let replies = vec![
                    length_reply("gzip", &gzip()),
                    chunked_reply("deflate", &zlib()),
                    chunked_reply("deflate", &RAW),
                    NO_CONTENT.as_bytes().to_vec(),
                ];
                let (client, server) = serve(Client::builder(), vec![wire(replies, chunk)]);
                let mut resp = client.get("http://h/").send().unwrap();
                assert_eq!(read_all(&mut resp).unwrap(), text(), "gzip by {chunk}");
                assert_eq!((resp.content_length(), resp.received()), (Some(56), 56));
                for name in ["zlib", "raw"] {
                    let mut resp = client.get("http://h/").send().unwrap();
                    assert_eq!(read_all(&mut resp).unwrap(), text(), "{name} by {chunk}");
                }
                assert_eq!(client.get("http://h/").send().unwrap().status(), 204);
                assert_eq!(server.opened(), 1);
            }
        });
    }

    #[test]
    fn bodies_asked_for_by_the_caller_stay_encoded() {
        in_task(|| {
            let (client, server) = serve(Client::builder(), vec![wire(vec![length_reply("gzip", &gzip())], 4096)]);
            let mut resp = client.get("http://h/").header("Accept-Encoding", "gzip").send().unwrap();
            assert_eq!(read_all(&mut resp).unwrap(), gzip());
            assert_eq!(server.sent(0).matches("Accept-Encoding").count(), 1);
        });
    }

    #[test]
    fn a_corrupt_body_fails() {
        in_task(|| {
            let mut bad = gzip();
            let n = bad.len();
            bad[n - 6] ^= 1;
            let (client, _) = serve(Client::builder(), vec![wire(vec![length_reply("gzip", &bad)], 4096)]);
            assert_eq!(read_all(&mut client.get("http://h/").send().unwrap()).err(), Some(Error::InvalidData));
            let cut = &gzip()[..40];
            let (client, _) = serve(Client::builder(), vec![wire(vec![length_reply("gzip", cut)], 4096)]);
            assert_eq!(read_all(&mut client.get("http://h/").send().unwrap()).err(), Some(Error::InvalidData));
        });
    }

    #[test]
    fn bytes_bounds_a_compressed_body() {
        in_task(|| {
            let reply = || length_reply("gzip", &gzip());
            let wires = vec![wire(vec![reply()], 4096), wire(vec![reply(), reply()], 4096)];
            let (client, server) = serve(Client::builder(), wires);
            assert_eq!(client.get("http://h/").send().unwrap().bytes(10).err(), Some(Error::MessageTooLarge));
            assert_eq!(client.get("http://h/").send().unwrap().bytes(100).err(), Some(Error::MessageTooLarge));
            assert_eq!(client.get("http://h/").send().unwrap().bytes(3300).unwrap(), text());
            assert_eq!(server.opened(), 2);
        });
    }
}
