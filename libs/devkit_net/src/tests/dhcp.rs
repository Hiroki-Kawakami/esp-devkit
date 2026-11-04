/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use idf::{executor, thread};

use super::{IP_B, Live, MAC_A, config, in_task};
use crate::dhcp::{self, ACK, CLIENT_PORT, DISCOVER, Message, NAK, OFFER, REQUEST, SERVER_PORT};
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

/// Leases `LEASED` for `lease` seconds; counts the renewals it saw. With
/// `Ignore`, it leases only once.
pub(super) fn serve(net: &'static Net, lease: u32, renewals: Renewals, run: Duration) -> Arc<AtomicUsize> {
    let renewed = Arc::new(AtomicUsize::new(0));
    let count = renewed.clone();
    let any = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, SERVER_PORT));
    let socket = UdpBuilder::default().bind_in(net, any).unwrap();
    thread::spawn(move || {
        let deadline = idf::time::Instant::now() + run;
        let mut buf = [0; 1024];
        let mut leased = false;
        while let Some(left) = deadline.checked_duration_since(idf::time::Instant::now()) {
            let Ok((n, _)) = socket.recv_from_timeout(&mut buf, left) else { break };
            let Some(m) = Message::decode(&buf[..n]) else { continue };
            let renewing = !m.ciaddr.is_unspecified();
            if renewing {
                count.fetch_add(1, Ordering::Relaxed);
                if renewals == Renewals::Ignore {
                    continue;
                }
            }
            if leased && renewals == Renewals::Ignore {
                continue;
            }
            let kind = match m.kind {
                DISCOVER => OFFER,
                REQUEST if renewing || m.requested == Some(LEASED) => ACK,
                REQUEST => NAK,
                _ => continue,
            };
            let reply = Message {
                op: 2,
                xid: m.xid,
                flags: m.flags,
                yiaddr: if kind == NAK { Ipv4Addr::UNSPECIFIED } else { LEASED },
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
    renewed
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

fn client(live: &Live, run: Duration) {
    let (net, iface) = (live.a, live.a_iface);
    thread::spawn(move || {
        let _ = executor::block_on(executor::timeout(run, dhcp::run(net, iface)));
    });
}

#[test]
fn leases_and_renews() {
    in_task(|| {
        let live = Live::with(None, config(IP_B));
        let renewed = serve(live.b, 4, Renewals::Answer, Duration::from_secs(4));
        client(&live, Duration::from_secs(4));
        let got = wait_address(&live, Duration::from_secs(2)).expect("no lease");
        assert_eq!(got, Ipv4Config { address: LEASED, prefix_len: 24, gateway: Some(ROUTER) });
        assert_eq!(live.a.lock().stack.dns_servers(live.a_iface), [DNS]);
        thread::sleep(Duration::from_millis(3000));
        assert!(renewed.load(Ordering::Relaxed) >= 1);
        assert_eq!(address(live.a, &live).map(|c| c.address), Some(LEASED));
    });
}

#[test]
fn unrenewed_lease_runs_out() {
    in_task(|| {
        let live = Live::with(None, config(IP_B));
        let renewed = serve(live.b, 2, Renewals::Ignore, Duration::from_secs(3));
        client(&live, Duration::from_secs(4));
        assert!(wait_address(&live, Duration::from_secs(1)).is_some());
        thread::sleep(Duration::from_millis(2500));
        assert!(renewed.load(Ordering::Relaxed) >= 1);
        assert_eq!(address(live.a, &live), None);
        assert!(live.a.lock().stack.dns_servers(live.a_iface).is_empty());
    });
}
