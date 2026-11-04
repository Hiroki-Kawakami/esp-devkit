/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use idf::{executor, thread};

use super::{IP_B, Live, MAC_A, config, in_task};
use crate::dhcp::{self, ACK, CLIENT_PORT, DECLINE, DISCOVER, Message, NAK, OFFER, RELEASE, REQUEST, SERVER_PORT};
use crate::net::Net;
use crate::{Ipv4Config, UdpBuilder};

pub(super) const LEASED: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 100);
const ROUTER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 254);
const DNS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 53);

#[test]
fn messages_round_trip() {
    let m = Message {
        op: 2,
        xid: 0x1234_5678,
        flags: 0x8000,
        yiaddr: LEASED,
        chaddr: MAC_A,
        kind: ACK,
        server: Some(IP_B),
        subnet: Some(Ipv4Addr::new(255, 255, 255, 0)),
        router: Some(ROUTER),
        dns: vec![DNS, ROUTER],
        lease: Some(3600),
        t1: Some(1800),
        secs: 7,
        hostname: b"devkit-test".to_vec(),
        max_size: Some(1500),
        ..Message::default()
    };
    assert_eq!(Message::decode(&m.encode()), Some(m));
    let request = Message { op: 1, kind: REQUEST, requested: Some(LEASED), ..Message::default() };
    let decoded = Message::decode(&request.encode()).unwrap();
    assert_eq!((decoded.kind, decoded.requested), (REQUEST, Some(LEASED)));
    assert_eq!(Message::decode(&request.encode()[..239]), None);
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Renewals {
    Answer,
    Ignore,
}

pub(super) const LEASED_TOO: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 101);

/// Leases `LEASED` for `lease` seconds, `LEASED_TOO` after a DECLINE, and
/// records every message it receives. With `Ignore`, it leases only once.
pub(super) fn serve(net: &'static Net, lease: u32, renewals: Renewals, run: Duration) -> Arc<Mutex<Vec<Message>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let any = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, SERVER_PORT));
    let socket = UdpBuilder::default().bind_in(net, any).unwrap();
    thread::spawn(move || {
        let deadline = idf::time::Instant::now() + run;
        let mut buf = [0; 1500];
        let mut leased = false;
        let mut offered = LEASED;
        while let Some(left) = deadline.checked_duration_since(idf::time::Instant::now()) {
            let Ok((n, _)) = socket.recv_from_timeout(&mut buf, left) else { break };
            let Some(m) = Message::decode(&buf[..n]) else { continue };
            log.lock().unwrap().push(m.clone());
            match m.kind {
                DECLINE => {
                    offered = LEASED_TOO;
                    continue;
                }
                RELEASE => continue,
                _ => {}
            }
            let renewing = !m.ciaddr.is_unspecified();
            if leased && renewals == Renewals::Ignore {
                continue;
            }
            let kind = match m.kind {
                DISCOVER => OFFER,
                REQUEST if renewing || m.requested == Some(offered) => ACK,
                REQUEST => NAK,
                _ => continue,
            };
            let reply = Message {
                op: 2,
                xid: m.xid,
                flags: m.flags,
                yiaddr: if kind == NAK { Ipv4Addr::UNSPECIFIED } else { offered },
                chaddr: m.chaddr,
                kind,
                server: Some(IP_B),
                subnet: Some(Ipv4Addr::new(255, 255, 255, 0)),
                router: Some(ROUTER),
                dns: vec![DNS],
                lease: Some(lease),
                ..Message::default()
            };
            leased |= kind == ACK;
            let to = if renewing { m.ciaddr } else { Ipv4Addr::BROADCAST };
            socket.send_to(&reply.encode(), SocketAddr::V4(SocketAddrV4::new(to, CLIENT_PORT))).unwrap();
        }
    });
    seen
}

fn renewals(seen: &Mutex<Vec<Message>>) -> usize {
    seen.lock().unwrap().iter().filter(|m| m.kind == REQUEST && !m.ciaddr.is_unspecified()).count()
}

fn address(net: &'static Net, live: &Live) -> Option<Ipv4Config> {
    net.lock().stack.ipv4(live.a_iface)
}

fn wait_address(live: &Live, within: Duration) -> Option<Ipv4Config> {
    let deadline = idf::time::Instant::now() + within;
    while idf::time::Instant::now() < deadline {
        if let Some(c) = address(live.a, live) {
            return Some(c);
        }
        thread::sleep(Duration::from_millis(10));
    }
    None
}

fn client(live: &Live, run: Duration) -> thread::JoinHandle<()> {
    let (net, iface) = (live.a, live.a_iface);
    thread::spawn(move || {
        let _ = executor::block_on(executor::timeout(run, dhcp::run(net, iface)));
    })
}

#[test]
fn leases_and_renews() {
    in_task(|| {
        let live = Live::with(None, config(IP_B));
        let seen = serve(live.b, 4, Renewals::Answer, Duration::from_secs(8));
        client(&live, Duration::from_secs(8));
        let got = wait_address(&live, Duration::from_secs(4)).expect("no lease");
        assert_eq!(got, Ipv4Config { address: LEASED, prefix_len: 24, gateway: Some(ROUTER) });
        assert_eq!(live.a.lock().stack.dns_servers(live.a_iface), [DNS]);
        thread::sleep(Duration::from_millis(3000));
        assert!(renewals(&seen) >= 1);
        assert_eq!(address(live.a, &live).map(|c| c.address), Some(LEASED));
        let discover = seen.lock().unwrap()[0].clone();
        assert_eq!((discover.kind, discover.max_size), (DISCOVER, Some(1500)));
    });
}

#[test]
fn unrenewed_lease_runs_out() {
    in_task(|| {
        let live = Live::with(None, config(IP_B));
        let seen = serve(live.b, 2, Renewals::Ignore, Duration::from_secs(6));
        client(&live, Duration::from_secs(6));
        assert!(wait_address(&live, Duration::from_secs(4)).is_some());
        thread::sleep(Duration::from_millis(2500));
        assert!(renewals(&seen) >= 1);
        assert_eq!(address(live.a, &live), None);
        assert!(live.a.lock().stack.dns_servers(live.a_iface).is_empty());
    });
}

#[test]
fn stopping_releases_and_restarting_asks_for_the_same_address() {
    in_task(|| {
        let live = Live::with(None, config(IP_B));
        let seen = serve(live.b, 3600, Renewals::Answer, Duration::from_secs(12));
        client(&live, Duration::from_secs(4)).join();
        assert_eq!(address(live.a, &live).map(|c| c.address), Some(LEASED));
        thread::sleep(Duration::from_millis(100));
        let release = seen.lock().unwrap().iter().find(|m| m.kind == RELEASE).cloned().expect("no RELEASE");
        assert_eq!((release.ciaddr, release.server), (LEASED, Some(IP_B)));

        live.a.lock().stack.set_ipv4(live.a_iface, None);
        let before = seen.lock().unwrap().len();
        client(&live, Duration::from_secs(4));
        assert!(wait_address(&live, Duration::from_secs(3)).is_some());
        let first = seen.lock().unwrap()[before].clone();
        assert_eq!((first.kind, first.requested, first.server, first.ciaddr), (REQUEST, Some(LEASED), None, Ipv4Addr::UNSPECIFIED));
    });
}

#[test]
fn taken_address_is_declined() {
    in_task(|| {
        let live = Live::with(None, config(LEASED));
        let seen = serve(live.b, 3600, Renewals::Answer, Duration::from_secs(5));
        client(&live, Duration::from_secs(5));
        thread::sleep(Duration::from_secs(4));
        let decline = seen.lock().unwrap().iter().find(|m| m.kind == DECLINE).cloned().expect("no DECLINE");
        assert_eq!(decline.requested, Some(LEASED));
        assert_eq!(address(live.a, &live), None);
    });
}
