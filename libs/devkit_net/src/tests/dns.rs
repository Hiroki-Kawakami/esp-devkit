/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use idf::{executor, thread};

use super::{IP_B, Live, in_task, v4};
use crate::dns;
use crate::dns::message::{
    self, CLASS_IN, FLAG_QR, FLAG_RD, FLAG_TC, HEADER, RCODE_NOERROR, RCODE_NXDOMAIN, TYPE_A, TYPE_CNAME, TYPE_SOA,
};
use crate::net::Net;
use crate::wire::put16;
use crate::{Error, TcpBuilder, UdpBuilder};

const ADDR_1: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 1);
const ADDR_2: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 2);
const NOBODY: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 9);
const RUN: Duration = Duration::from_secs(8);
const RCODE_SERVFAIL: u8 = 2;
const FLAG_RA: u16 = 0x0080;

enum Data {
    A(Ipv4Addr),
    Cname(&'static str),
    /// The SOA's MINIMUM.
    Soa(u32),
}

struct Rr {
    owner: &'static str,
    ttl: u32,
    data: Data,
}

fn a(owner: &'static str, addr: Ipv4Addr) -> Rr {
    Rr { owner, ttl: 60, data: Data::A(addr) }
}

fn cname(owner: &'static str, target: &'static str) -> Rr {
    Rr { owner, ttl: 60, data: Data::Cname(target) }
}

fn soa(owner: &'static str, minimum: u32) -> Rr {
    Rr { owner, ttl: 3600, data: Data::Soa(minimum) }
}

fn question(query: &[u8]) -> String {
    let mut name = Vec::new();
    let mut at = HEADER;
    while query[at] != 0 {
        let len = usize::from(query[at]);
        if !name.is_empty() {
            name.push(b'.');
        }
        name.extend_from_slice(&query[at + 1..at + 1 + len]);
        at += 1 + len;
    }
    String::from_utf8(name).unwrap()
}

/// A response to `query`; records owned by the asked name point back at the
/// question.
fn response(query: &[u8], rcode: u8, truncated: bool, answers: &[Rr], authority: &[Rr]) -> Vec<u8> {
    let asked = question(query);
    let end = HEADER + asked.len() + 2 + 4;
    let mut out = query[..end].to_vec();
    let flags = FLAG_QR | FLAG_RD | FLAG_RA | if truncated { FLAG_TC } else { 0 } | u16::from(rcode);
    put16(&mut out, 2, flags);
    put16(&mut out, 6, answers.len() as u16);
    put16(&mut out, 8, authority.len() as u16);
    for rr in answers.iter().chain(authority) {
        if rr.owner == asked {
            out.extend_from_slice(&[0xc0, HEADER as u8]);
        } else {
            message::put_name(&mut out, rr.owner.as_bytes());
        }
        let mut rdata = Vec::new();
        let kind = match rr.data {
            Data::A(addr) => {
                rdata.extend_from_slice(&addr.octets());
                TYPE_A
            }
            Data::Cname(target) => {
                message::put_name(&mut rdata, target.as_bytes());
                TYPE_CNAME
            }
            Data::Soa(minimum) => {
                message::put_name(&mut rdata, b"ns.example");
                message::put_name(&mut rdata, b"admin.example");
                for v in [1u32, 3600, 600, 86400, minimum] {
                    rdata.extend_from_slice(&v.to_be_bytes());
                }
                TYPE_SOA
            }
        };
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&rr.ttl.to_be_bytes());
        out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        out.extend_from_slice(&rdata);
    }
    out
}

type Handler = dyn Fn(&[u8], usize, SocketAddr) -> Vec<Vec<u8>> + Send + Sync;

/// A name server on port 53 of `net`: `handler` gets each query, how many
/// came before it and its sender, and returns the datagrams to answer with.
/// The names asked for are recorded.
fn serve(
    net: &'static Net,
    handler: impl Fn(&[u8], usize, SocketAddr) -> Vec<Vec<u8>> + Send + Sync + 'static,
) -> Arc<Mutex<Vec<String>>> {
    let handler: Arc<Handler> = Arc::new(handler);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    let socket = UdpBuilder::default().bind_in(net, v4(Ipv4Addr::UNSPECIFIED, message::PORT)).unwrap();
    thread::spawn(move || {
        let deadline = idf::time::Instant::now() + RUN;
        let mut buf = [0; 1500];
        while let Some(left) = deadline.checked_duration_since(idf::time::Instant::now()) {
            let Ok((n, from)) = socket.recv_from_timeout(&mut buf, left) else { break };
            let count = {
                let mut log = log.lock().unwrap();
                log.push(question(&buf[..n]));
                log.len() - 1
            };
            for reply in handler(&buf[..n], count, from) {
                socket.send_to(&reply, from).unwrap();
            }
        }
    });
    asked
}

fn client(live: &Live, servers: &[Ipv4Addr]) {
    let spawner = thread::Builder::new().name("dns").spawn_local_executor(|_| {}).unwrap();
    live.a.set_spawner(spawner);
    live.a.lock().stack.set_dns_servers(live.a_iface, servers);
}

fn lookup(live: &Live, host: &str) -> crate::Result<Vec<IpAddr>> {
    executor::block_on(dns::lookup_in(live.a, host))
}

fn ips(addrs: &[Ipv4Addr]) -> Vec<IpAddr> {
    addrs.iter().map(|&a| IpAddr::V4(a)).collect()
}

#[test]
fn names_parse_and_decode() {
    assert_eq!(message::parse_name("Host.Example."), Some(b"host.example".to_vec()));
    assert_eq!(message::parse_name("bad..example"), None);
    assert_eq!(message::parse_name(&"x".repeat(64)), None);
    assert_eq!(message::parse_name("sp ace.example"), None);

    let query = message::query(0x1234, b"www.example");
    let reply =
        response(&query, RCODE_NOERROR, false, &[cname("www.example", "cdn.example"), a("cdn.example", ADDR_1)], &[]);
    let decoded = message::decode(&reply).unwrap();
    assert_eq!((decoded.id, decoded.question.as_slice()), (0x1234, &b"www.example"[..]));
    assert_eq!(decoded.cnames, vec![(b"www.example".to_vec(), b"cdn.example".to_vec(), 60)]);
    assert_eq!(decoded.addrs, vec![(b"cdn.example".to_vec(), ADDR_1, 60)]);

    let reply = response(&query, RCODE_NXDOMAIN, false, &[], &[soa("example", 30)]);
    assert_eq!(message::decode(&reply).unwrap().negative_ttl, Some(30));
    assert!(message::decode(&query).is_none());

    let mut looped = response(&query, RCODE_NOERROR, false, &[], &[]);
    put16(&mut looped, 6, 1);
    let at = looped.len() as u16;
    looped.extend_from_slice(&[0xc0 | (at >> 8) as u8, at as u8]);
    looped.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 1, 2, 3, 4]);
    assert!(message::decode(&looped).is_none());
}

#[test]
fn resolves_and_caches() {
    in_task(|| {
        let live = Live::new();
        let asked = serve(live.b, |q, _, _| {
            vec![response(q, RCODE_NOERROR, false, &[a("host.example", ADDR_1), a("host.example", ADDR_2)], &[])]
        });
        client(&live, &[IP_B]);
        assert_eq!(lookup(&live, "Host.Example").unwrap(), ips(&[ADDR_1, ADDR_2]));
        assert_eq!(lookup(&live, "host.example.").unwrap(), ips(&[ADDR_1, ADDR_2]));
        assert_eq!(*asked.lock().unwrap(), ["host.example"]);
    });
}

#[test]
fn follows_cnames_and_caches_each_step() {
    in_task(|| {
        let live = Live::new();
        let asked = serve(live.b, |q, _, _| {
            vec![response(
                q,
                RCODE_NOERROR,
                false,
                &[cname("www.example", "cdn.example"), a("cdn.example", ADDR_1)],
                &[],
            )]
        });
        client(&live, &[IP_B]);
        assert_eq!(lookup(&live, "www.example").unwrap(), ips(&[ADDR_1]));
        assert_eq!(lookup(&live, "cdn.example").unwrap(), ips(&[ADDR_1]));
        assert_eq!(lookup(&live, "www.example").unwrap(), ips(&[ADDR_1]));
        assert_eq!(*asked.lock().unwrap(), ["www.example"]);
    });
}

#[test]
fn asks_again_where_an_alias_chain_stops() {
    in_task(|| {
        let live = Live::new();
        let asked = serve(live.b, |q, _, _| match question(q).as_str() {
            "www.example" => vec![response(q, RCODE_NOERROR, false, &[cname("www.example", "cdn.example")], &[])],
            _ => vec![response(q, RCODE_NOERROR, false, &[a("cdn.example", ADDR_2)], &[])],
        });
        client(&live, &[IP_B]);
        assert_eq!(lookup(&live, "www.example").unwrap(), ips(&[ADDR_2]));
        assert_eq!(*asked.lock().unwrap(), ["www.example", "cdn.example"]);
    });
}

#[test]
fn alias_loops_end_as_not_found() {
    in_task(|| {
        let live = Live::new();
        serve(live.b, |q, _, _| match question(q).as_str() {
            "a.example" => vec![response(q, RCODE_NOERROR, false, &[cname("a.example", "b.example")], &[])],
            _ => vec![response(q, RCODE_NOERROR, false, &[cname("b.example", "a.example")], &[])],
        });
        client(&live, &[IP_B]);
        assert_eq!(lookup(&live, "a.example"), Err(Error::NotFound));
    });
}

#[test]
fn missing_names_are_cached() {
    in_task(|| {
        let live = Live::new();
        let asked = serve(live.b, |q, _, _| match question(q).as_str() {
            "gone.example" => vec![response(q, RCODE_NXDOMAIN, false, &[], &[soa("example", 30)])],
            _ => vec![response(q, RCODE_NOERROR, false, &[], &[soa("example", 30)])],
        });
        client(&live, &[IP_B]);
        for _ in 0..2 {
            assert_eq!(lookup(&live, "gone.example"), Err(Error::NotFound));
            assert_eq!(lookup(&live, "noaddr.example"), Err(Error::NotFound));
        }
        assert_eq!(*asked.lock().unwrap(), ["gone.example", "noaddr.example"]);
    });
}

#[test]
fn server_failure_asks_again_at_once() {
    in_task(|| {
        let live = Live::new();
        let asked = serve(live.b, |q, count, _| {
            let rcode = if count == 0 { RCODE_SERVFAIL } else { RCODE_NOERROR };
            vec![response(q, rcode, false, &[a("host.example", ADDR_1)], &[])]
        });
        client(&live, &[IP_B]);
        let start = idf::time::Instant::now();
        assert_eq!(lookup(&live, "host.example").unwrap(), ips(&[ADDR_1]));
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(asked.lock().unwrap().len(), 2);
    });
}

#[test]
fn forged_replies_are_ignored() {
    in_task(|| {
        let live = Live::new();
        let other = UdpBuilder::default().bind_in(live.b, v4(Ipv4Addr::UNSPECIFIED, 5353)).unwrap();
        serve(live.b, move |q, _, from| {
            let forged = response(q, RCODE_NOERROR, false, &[a("host.example", ADDR_2)], &[]);
            other.send_to(&forged, from).unwrap();
            let mut wrong_id = forged.clone();
            wrong_id[0] ^= 0xff;
            let mut wrong_name = forged;
            wrong_name[HEADER + 1] = b'x';
            vec![wrong_id, wrong_name, response(q, RCODE_NOERROR, false, &[a("host.example", ADDR_1)], &[])]
        });
        client(&live, &[IP_B]);
        assert_eq!(lookup(&live, "host.example").unwrap(), ips(&[ADDR_1]));
    });
}

#[test]
fn truncated_answers_are_asked_again_over_tcp() {
    in_task(|| {
        let live = Live::new();
        serve(live.b, |q, _, _| vec![response(q, RCODE_NOERROR, true, &[], &[])]);
        let listener = TcpBuilder::default().listen_in(live.b, v4(Ipv4Addr::UNSPECIFIED, message::PORT)).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut len = [0u8; 2];
            let mut got = 0;
            while got < 2 {
                got += stream.read(&mut len[got..]).unwrap();
            }
            let mut query = vec![0u8; usize::from(u16::from_be_bytes(len))];
            let mut got = 0;
            while got < query.len() {
                got += stream.read(&mut query[got..]).unwrap();
            }
            let reply = response(&query, RCODE_NOERROR, false, &[a("big.example", ADDR_2)], &[]);
            stream.write_all(&(reply.len() as u16).to_be_bytes()).unwrap();
            stream.write_all(&reply).unwrap();
            let mut rest = [0u8; 1];
            let _ = stream.read_timeout(&mut rest, Duration::from_secs(2));
        });
        client(&live, &[IP_B]);
        assert_eq!(lookup(&live, "big.example").unwrap(), ips(&[ADDR_2]));
    });
}

#[test]
fn a_silent_server_hands_over_to_the_next() {
    in_task(|| {
        let live = Live::new();
        serve(live.b, |q, _, _| vec![response(q, RCODE_NOERROR, false, &[a("host.example", ADDR_1)], &[])]);
        client(&live, &[NOBODY, IP_B]);
        let start = idf::time::Instant::now();
        assert_eq!(lookup(&live, "host.example").unwrap(), ips(&[ADDR_1]));
        assert!(start.elapsed() < Duration::from_secs(3));
    });
}

#[test]
fn no_answer_times_out() {
    in_task(|| {
        let live = Live::new();
        client(&live, &[NOBODY]);
        let start = idf::time::Instant::now();
        assert_eq!(lookup(&live, "host.example"), Err(Error::TimedOut));
        assert!(start.elapsed() >= Duration::from_secs(9));
    });
}

#[test]
fn lookups_of_one_name_share_a_query() {
    in_task(|| {
        let live = Live::new();
        let asked = serve(live.b, |q, _, _| {
            thread::sleep(Duration::from_millis(200));
            vec![response(q, RCODE_NOERROR, false, &[a("host.example", ADDR_1)], &[])]
        });
        client(&live, &[IP_B]);
        let net = live.a;
        let other = thread::spawn(move || executor::block_on(dns::lookup_in(net, "host.example")));
        assert_eq!(lookup(&live, "host.example").unwrap(), ips(&[ADDR_1]));
        assert_eq!(other.join().unwrap(), ips(&[ADDR_1]));
        assert_eq!(asked.lock().unwrap().len(), 1);
    });
}

#[test]
fn new_servers_clear_the_cache() {
    in_task(|| {
        let live = Live::new();
        let asked = serve(live.b, |q, _, _| vec![response(q, RCODE_NOERROR, false, &[a("host.example", ADDR_1)], &[])]);
        client(&live, &[IP_B]);
        lookup(&live, "host.example").unwrap();
        live.a.lock().stack.set_dns_servers(live.a_iface, &[IP_B, NOBODY]);
        lookup(&live, "host.example").unwrap();
        assert_eq!(asked.lock().unwrap().len(), 2);
    });
}

#[test]
fn literals_and_bad_names_need_no_server() {
    in_task(|| {
        let live = Live::new();
        client(&live, &[]);
        assert_eq!(lookup(&live, "192.0.2.7").unwrap(), ips(&[Ipv4Addr::new(192, 0, 2, 7)]));
        assert_eq!(lookup(&live, "localhost").unwrap(), ips(&[Ipv4Addr::LOCALHOST]));
        assert_eq!(lookup(&live, "bad..example"), Err(Error::InvalidInput));
        assert_eq!(lookup(&live, "host.example"), Err(Error::NoNameServer));
    });
}

#[test]
fn sockets_connect_by_name() {
    in_task(|| {
        let live = Live::new();
        serve(live.b, |q, _, _| vec![response(q, RCODE_NOERROR, false, &[a("peer.example", IP_B)], &[])]);
        client(&live, &[IP_B]);
        let listener = TcpBuilder::default().listen_in(live.b, v4(Ipv4Addr::UNSPECIFIED, 7)).unwrap();
        let accept = thread::spawn(move || {
            for _ in 0..2 {
                drop(listener.accept().unwrap());
            }
        });
        let stream = executor::block_on(TcpBuilder::default().connect_in(live.a, ("peer.example", 7))).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), v4(IP_B, 7));
        let stream = executor::block_on(TcpBuilder::default().connect_in(live.a, "peer.example:7")).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), v4(IP_B, 7));
        accept.join();

        let socket = UdpBuilder::default().bind_in(live.a, v4(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        socket.connect("peer.example:9").unwrap();
        assert_eq!(socket.peer_addr().unwrap(), v4(IP_B, 9));
        assert_eq!(socket.connect("peer.example"), Err(Error::InvalidInput));
    });
}
