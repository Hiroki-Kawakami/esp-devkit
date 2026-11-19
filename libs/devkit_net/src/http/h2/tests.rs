/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{PREFACE, frame, hpack};
use crate::Error;
use crate::http::{Client, Started};
use crate::tests::http::{Wire, read_all, serve};
use crate::tests::in_task;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Event {
    /// A request's header block, its body still to come.
    Headers(u32),
    /// A whole request.
    Request(u32),
}

#[derive(Default)]
struct Request {
    fields: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

type Handler = Box<dyn FnMut(&mut Server, Event) + Send>;

/// The server end of one HTTP/2 connection, keeping to the client's windows.
struct Server {
    buf: Vec<u8>,
    preface: bool,
    hpack: hpack::Decoder,
    block: Option<(u32, Vec<u8>, bool)>,
    requests: HashMap<u32, Request>,
    order: Vec<u32>,
    out: Vec<u8>,
    max_streams: u32,
    piece: usize,
    send_window: i64,
    peer_window: i64,
    windows: HashMap<u32, i64>,
    pending: Vec<(u32, Vec<u8>)>,
    resets: Vec<(u32, u32)>,
    pings: Vec<[u8; 8]>,
    handler: Option<Handler>,
}

impl Server {
    fn new(handler: impl FnMut(&mut Server, Event) + Send + 'static) -> Self {
        Self {
            buf: Vec::new(),
            preface: false,
            hpack: hpack::Decoder::new(4096),
            block: None,
            requests: HashMap::new(),
            order: Vec::new(),
            out: Vec::new(),
            max_streams: 100,
            piece: 16384,
            send_window: 65535,
            peer_window: 65535,
            windows: HashMap::new(),
            pending: Vec::new(),
            resets: Vec::new(),
            pings: Vec::new(),
            handler: Some(Box::new(handler)),
        }
    }

    fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(data);
        if !self.preface {
            if self.buf.len() < PREFACE.len() {
                return Vec::new();
            }
            assert_eq!(&self.buf[..PREFACE.len()], PREFACE);
            self.buf.drain(..PREFACE.len());
            self.preface = true;
            frame::settings(&mut self.out, &[(frame::SETTINGS_MAX_CONCURRENT_STREAMS, self.max_streams)]);
        }
        while self.buf.len() >= frame::HEADER {
            let (len, kind, flags, id) = frame::parse(&self.buf);
            if self.buf.len() < frame::HEADER + len {
                break;
            }
            let payload: Vec<u8> = self.buf.drain(..frame::HEADER + len).skip(frame::HEADER).collect();
            self.frame(kind, flags, id, &payload);
        }
        self.pump();
        std::mem::take(&mut self.out)
    }

    fn event(&mut self, event: Event) {
        let mut handler = self.handler.take().unwrap();
        handler(self, event);
        self.handler = Some(handler);
    }

    fn frame(&mut self, kind: u8, flags: u8, id: u32, p: &[u8]) {
        let word = |i: usize| u32::from_be_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]);
        match kind {
            frame::SETTINGS if flags & frame::ACK == 0 => {
                for e in p.chunks_exact(6) {
                    if u16::from_be_bytes([e[0], e[1]]) == frame::SETTINGS_INITIAL_WINDOW_SIZE {
                        self.peer_window = i64::from(u32::from_be_bytes([e[2], e[3], e[4], e[5]]));
                    }
                }
                frame::header(&mut self.out, 0, frame::SETTINGS, frame::ACK, 0);
            }
            frame::WINDOW_UPDATE => {
                let n = i64::from(word(0));
                if id == 0 {
                    self.send_window += n;
                } else {
                    *self.windows.entry(id).or_insert(self.peer_window) += n;
                }
            }
            frame::HEADERS | frame::CONTINUATION => {
                let (data, end) = match self.block.take() {
                    Some((_, mut data, end)) => {
                        data.extend_from_slice(p);
                        (data, end)
                    }
                    None => (p.to_vec(), flags & frame::END_STREAM != 0),
                };
                if flags & frame::END_HEADERS == 0 {
                    self.block = Some((id, data, end));
                    return;
                }
                let fields = self.hpack.decode(&data, 1 << 20).unwrap();
                self.requests.insert(id, Request { fields, body: Vec::new() });
                self.order.push(id);
                self.event(if end { Event::Request(id) } else { Event::Headers(id) });
            }
            frame::DATA => {
                self.requests.get_mut(&id).unwrap().body.extend_from_slice(p);
                if !p.is_empty() {
                    frame::window_update(&mut self.out, 0, p.len() as u32);
                    frame::window_update(&mut self.out, id, p.len() as u32);
                }
                if flags & frame::END_STREAM != 0 {
                    self.event(Event::Request(id));
                }
            }
            frame::RST_STREAM => self.resets.push((id, word(0))),
            frame::PING => self.pings.push(p.try_into().unwrap()),
            _ => {}
        }
    }

    fn head(&mut self, id: u32, status: u16, fields: &[(&str, &str)], end: bool) {
        let mut block = Vec::new();
        hpack::encode(&mut block, ":status", &status.to_string());
        for (n, v) in fields {
            hpack::encode(&mut block, n, v);
        }
        frame::headers(&mut self.out, id, &block, end, self.piece);
    }

    fn respond(&mut self, id: u32, status: u16, fields: &[(&str, &str)], body: &[u8]) {
        self.head(id, status, fields, body.is_empty());
        if !body.is_empty() {
            self.pending.push((id, body.to_vec()));
        }
    }

    fn pump(&mut self) {
        let mut i = 0;
        while i < self.pending.len() {
            let id = self.pending[i].0;
            let stream = *self.windows.entry(id).or_insert(self.peer_window);
            let room = self.send_window.min(stream).min(16384);
            if room <= 0 {
                i += 1;
                continue;
            }
            let body = &mut self.pending[i].1;
            let n = (room as usize).min(body.len());
            let piece: Vec<u8> = body.drain(..n).collect();
            let last = body.is_empty();
            frame::header(&mut self.out, n, frame::DATA, if last { frame::END_STREAM } else { 0 }, id);
            self.out.extend_from_slice(&piece);
            self.send_window -= n as i64;
            *self.windows.get_mut(&id).unwrap() -= n as i64;
            if last {
                self.pending.remove(i);
            }
        }
    }
}

fn h2_client(servers: Vec<Server>) -> (Client, Arc<crate::tests::http::Server>, Vec<Arc<Mutex<Server>>>) {
    let servers: Vec<_> = servers.into_iter().map(|s| Arc::new(Mutex::new(s))).collect();
    let wires = servers
        .iter()
        .map(|s| {
            let s = s.clone();
            Wire::h2(move |bytes| s.lock().unwrap().feed(bytes))
        })
        .collect();
    let (client, wires) = serve(Client::builder(), wires);
    (client, wires, servers)
}

fn hello(srv: &mut Server, event: Event) {
    if let Event::Request(id) = event {
        srv.respond(id, 200, &[("content-length", "5")], b"hello");
    }
}

#[test]
fn requests_go_out_as_header_blocks_and_share_the_connection() {
    in_task(|| {
        let (client, wires, servers) = h2_client(vec![Server::new(hello)]);
        let mut resp = client.get("https://example.com/a?b").header("X-Test", "1").send().unwrap();
        assert_eq!((resp.status(), resp.content_length(), resp.url()), (200, Some(5), "https://example.com/a?b"));
        assert_eq!(resp.version(), crate::http::Version::Http2);
        assert_eq!(read_all(&mut resp).unwrap(), b"hello");
        assert_eq!(read_all(&mut client.get("https://example.com/c").send().unwrap()).unwrap(), b"hello");
        assert_eq!(wires.opened(), 1);
        let srv = servers[0].lock().unwrap();
        assert_eq!(srv.order, [1, 3]);
        let req = &srv.requests[&1];
        let pseudo: Vec<_> = req.fields.iter().take(4).map(|(n, v)| format!("{n}={v}")).collect();
        assert_eq!(pseudo, [":method=GET", ":scheme=https", ":authority=example.com", ":path=/a?b"]);
        assert_eq!(req.field("x-test"), Some("1"));
        assert_eq!(req.field("user-agent"), Some("devkit_net"));
        assert!(req.field("host").is_none());
        assert_eq!(req.field("accept-encoding").is_some(), cfg!(CONFIG_DEVKIT_NET_HTTP_DECOMPRESS));
    });
}

#[test]
fn concurrent_streams_get_their_own_responses() {
    in_task(|| {
        let reversed = |srv: &mut Server, event: Event| {
            let Event::Request(id) = event else { return };
            let path = |s: &Server, id: u32| s.requests[&id].field(":path").unwrap().to_string();
            if path(srv, id) == "/warm" {
                return srv.respond(id, 200, &[], b"");
            }
            if srv.order.len() == 3 {
                let (a, b) = (srv.order[1], srv.order[2]);
                let (pa, pb) = (path(srv, a), path(srv, b));
                srv.respond(b, 200, &[], pb.as_bytes());
                srv.respond(a, 200, &[], pa.as_bytes());
            }
        };
        let (client, wires, _) = h2_client(vec![Server::new(reversed)]);
        assert_eq!(client.get("https://h/warm").send().unwrap().status(), 200);
        let threads: Vec<_> = ["/one", "/two"]
            .into_iter()
            .map(|path| {
                let client = client.clone();
                idf::thread::spawn(move || {
                    let mut resp = client.get(&format!("https://h{path}")).send().unwrap();
                    assert_eq!(read_all(&mut resp).unwrap(), path.as_bytes());
                })
            })
            .collect();
        for t in threads {
            t.join();
        }
        assert_eq!(wires.opened(), 1);
    });
}

#[test]
fn streams_wait_for_a_slot_the_server_allows() {
    in_task(|| {
        let mut srv = Server::new(hello);
        srv.max_streams = 1;
        let (client, _, _) = h2_client(vec![srv]);
        let mut first = client.get("https://h/").send().unwrap();
        let blocked = client.get("https://h/").timeout(Some(Duration::from_millis(300))).send();
        assert_eq!(blocked.err(), Some(Error::TimedOut));
        assert_eq!(read_all(&mut first).unwrap(), b"hello");
        assert_eq!(read_all(&mut client.get("https://h/").send().unwrap()).unwrap(), b"hello");
    });
}

#[test]
fn large_bodies_keep_to_the_windows_both_ways() {
    in_task(|| {
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let body = big.clone();
        let echo = move |srv: &mut Server, event: Event| {
            if let Event::Request(id) = event {
                let got = std::mem::take(&mut srv.requests.get_mut(&id).unwrap().body);
                let reply = if got.is_empty() { body.clone() } else { got.len().to_string().into_bytes() };
                srv.respond(id, 200, &[], &reply);
            }
        };
        let (client, _, servers) = h2_client(vec![Server::new(echo)]);
        let mut resp = client.get("https://h/big").send().unwrap();
        assert_eq!(read_all(&mut resp).unwrap(), big);
        assert_eq!(resp.received(), big.len() as u64);
        let Started::Body(mut up) = client.put("https://h/up").content_length(big.len() as u64).start().unwrap() else {
            panic!("no body asked")
        };
        for piece in big.chunks(50_000) {
            up.write_all(piece).unwrap();
        }
        assert_eq!(up.finish().unwrap().bytes(16).unwrap(), b"300000");
        let srv = servers[0].lock().unwrap();
        assert_eq!(srv.requests[&3].field("content-length"), Some("300000"));
    });
}

#[test]
fn a_streamed_body_waits_for_continue() {
    in_task(|| {
        let handler = |srv: &mut Server, event: Event| match event {
            Event::Headers(id) if srv.requests[&id].field(":path") == Some("/no") => srv.respond(id, 413, &[], b""),
            Event::Headers(id) => srv.head(id, 100, &[], false),
            Event::Request(id) => {
                let n = srv.requests[&id].body.len().to_string();
                srv.respond(id, 201, &[], n.as_bytes());
            }
        };
        let (client, wires, servers) = h2_client(vec![Server::new(handler)]);
        let Started::Body(mut up) = client.post("https://h/yes").start().unwrap() else { panic!("no body asked") };
        up.write_all(b"hello").unwrap();
        assert_eq!(up.finish().unwrap().bytes(16).unwrap(), b"5");
        let Started::Response(no) = client.post("https://h/no").start().unwrap() else { panic!("body asked") };
        assert_eq!(no.status(), 413);
        drop(no);
        assert_eq!(read_all(&mut client.get("https://h/").send().unwrap()).unwrap(), b"0");
        assert_eq!(wires.opened(), 1);
        let srv = servers[0].lock().unwrap();
        assert_eq!(srv.requests[&1].field("expect"), Some("100-continue"));
        assert!(srv.requests[&1].field("transfer-encoding").is_none());
        assert!(srv.resets.contains(&(3, frame::CANCEL)), "{:?}", srv.resets);
    });
}

#[test]
fn refused_streams_and_goaway_send_the_request_again() {
    in_task(|| {
        let refuse_first = |srv: &mut Server, event: Event| {
            if let Event::Request(id) = event {
                if id == 1 {
                    frame::rst_stream(&mut srv.out, id, 7);
                } else {
                    hello(srv, event);
                    frame::goaway(&mut srv.out, id, 0);
                }
            }
        };
        let (client, wires, servers) = h2_client(vec![Server::new(refuse_first), Server::new(hello)]);
        assert_eq!(read_all(&mut client.get("https://h/").send().unwrap()).unwrap(), b"hello");
        assert_eq!(servers[0].lock().unwrap().order, [1, 3]);
        assert_eq!(read_all(&mut client.get("https://h/").send().unwrap()).unwrap(), b"hello");
        assert_eq!(wires.opened(), 2);
    });
}

#[test]
fn a_dropped_response_resets_its_stream() {
    in_task(|| {
        let big = |srv: &mut Server, event: Event| {
            if let Event::Request(id) = event {
                srv.respond(id, 200, &[], &vec![7u8; 200_000]);
            }
        };
        let (client, wires, servers) = h2_client(vec![Server::new(big)]);
        let mut resp = client.get("https://h/").send().unwrap();
        resp.read(&mut [0; 10]).unwrap();
        drop(resp);
        assert_eq!(read_all(&mut client.get("https://h/").send().unwrap()).unwrap().len(), 200_000);
        assert_eq!(wires.opened(), 1);
        assert_eq!(servers[0].lock().unwrap().resets, [(1, frame::CANCEL)]);
    });
}

#[test]
fn control_frames_and_split_header_blocks() {
    in_task(|| {
        let handler = |srv: &mut Server, event: Event| {
            if let Event::Request(id) = event {
                frame::header(&mut srv.out, 8, frame::PING, 0, 0);
                srv.out.extend_from_slice(b"pingpong");
                srv.piece = 10;
                let long = "v".repeat(100);
                srv.respond(id, 200, &[("x-long", &long), ("content-length", "0")], b"");
            }
        };
        let (client, _, servers) = h2_client(vec![Server::new(handler)]);
        let mut head = client.head("https://h/").send().unwrap();
        assert_eq!(head.headers().get("x-long").map(str::len), Some(100));
        assert_eq!(head.bytes(10).unwrap(), b"");
        client.get("https://h/").send().unwrap();
        assert!(servers[0].lock().unwrap().pings.contains(b"pingpong"));
    });
}

#[test]
fn a_protocol_error_ends_the_connection() {
    in_task(|| {
        let push = |srv: &mut Server, event: Event| {
            if let Event::Request(id) = event {
                frame::header(&mut srv.out, 4, frame::PUSH_PROMISE, frame::END_HEADERS, id);
                srv.out.extend_from_slice(&2u32.to_be_bytes());
            }
        };
        let (client, wires, _) = h2_client(vec![Server::new(push), Server::new(push)]);
        assert_eq!(client.get("https://h/").send().err(), Some(Error::InvalidData));
        assert_eq!(wires.opened(), 1);
    });
}
