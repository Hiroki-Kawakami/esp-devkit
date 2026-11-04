/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::net::Ipv4Addr;
use std::time::Duration;

use crate::harness::{End, Net};
use crate::wire::{
    ARP_LEN, ARP_REPLY, ARP_REQUEST, Arp, BROADCAST_MAC, ETHERNET_HEADER, ETHERTYPE_ARP, ETHERTYPE_IPV4, Ethernet,
    ICMP_ECHO_REPLY, ICMP_ECHO_REQUEST, IPV4_HEADER, Ipv4, Mac, PROTO_ICMP, checksum, put16, sum,
};
use crate::{Error, IfaceId, Instant, Ipv4Config, LinkInfo, Medium, Stack};

const MAC_A: Mac = [0x02, 0, 0, 0, 0, 0x0a];
const MAC_B: Mac = [0x02, 0, 0, 0, 0, 0x0b];
const IP_A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
const IP_B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
const GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 254);
const REMOTE: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);
const WIRE: Duration = Duration::from_millis(1);

fn ms(ms: u64) -> Instant {
    Instant::from_micros(ms * 1000)
}

fn config(address: Ipv4Addr) -> Option<Ipv4Config> {
    Some(Ipv4Config { address, prefix_len: 24, gateway: Some(GATEWAY) })
}

fn host(net: &mut Net, mac: Mac, ip: Ipv4Addr) -> End {
    let mut stack = Stack::new();
    let id = stack.add_interface("eth", LinkInfo { medium: Medium::Ethernet, mac, mtu: 1500 });
    stack.set_ipv4(id, config(ip));
    (net.add_stack(stack), id)
}

fn pair() -> (Net, End, End) {
    let mut net = Net::new();
    let a = host(&mut net, MAC_A, IP_A);
    let b = host(&mut net, MAC_B, IP_B);
    net.connect(a, b, WIRE);
    (net, a, b)
}

fn icmp(kind: u8, ident: u16, seq: u16, data: &[u8]) -> Vec<u8> {
    let mut p = vec![kind, 0, 0, 0];
    p.extend_from_slice(&ident.to_be_bytes());
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(data);
    let c = checksum(sum(&p, 0));
    put16(&mut p, 2, c);
    p
}

fn ipv4(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0; IPV4_HEADER];
    Ipv4 { src, dst, protocol, ttl: 64, ident: 1, frag: 0 }.write_header(&mut p, payload.len());
    p.extend_from_slice(payload);
    p
}

fn ethernet(dst: Mac, src: Mac, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0; ETHERNET_HEADER];
    Ethernet::write_header(&mut f, dst, src, ethertype);
    f.extend_from_slice(payload);
    f
}

fn echo_request(dst_mac: Mac, dst: Ipv4Addr, seq: u16) -> Vec<u8> {
    let request = icmp(ICMP_ECHO_REQUEST, 0x1234, seq, b"ping data");
    ethernet(dst_mac, MAC_A, ETHERTYPE_IPV4, &ipv4(IP_A, dst, PROTO_ICMP, &request))
}

fn arp_of(frame: &[u8]) -> Option<(Mac, Arp)> {
    let eth = Ethernet::parse(frame)?;
    (eth.ethertype == ETHERTYPE_ARP).then(|| (eth.dst, Arp::parse(eth.payload).unwrap()))
}

fn icmp_of(frame: &[u8]) -> Option<(Mac, Ipv4, Vec<u8>)> {
    let eth = Ethernet::parse(frame)?;
    if eth.ethertype != ETHERTYPE_IPV4 {
        return None;
    }
    let (ip, payload) = Ipv4::parse(eth.payload).unwrap();
    (ip.protocol == PROTO_ICMP).then(|| (eth.dst, ip, payload.to_vec()))
}

/// Leaves out gratuitous ARP, which every configured interface sends.
fn frames_from(net: &Net, from: End) -> Vec<&[u8]> {
    net.captured
        .iter()
        .filter(|c| c.from == from && !arp_of(&c.frame).is_some_and(|(_, arp)| arp.spa == arp.tpa))
        .map(|c| &c.frame[..])
        .collect()
}

#[test]
fn checksum_matches_rfc1071() {
    let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
    assert_eq!(checksum(sum(&data, 0)), !0xddf2);
    assert_eq!(checksum(sum(&[0x12, 0x34, 0x56], 0)), !(0x1234u16 + 0x5600));
}

#[test]
fn ipv4_parse_checks_header() {
    let mut p = ipv4(IP_A, IP_B, PROTO_ICMP, &[1, 2, 3, 4]);
    p.extend_from_slice(&[0; 6]);
    let (ip, payload) = Ipv4::parse(&p).unwrap();
    assert_eq!((ip.src, ip.dst, ip.protocol, ip.is_fragment()), (IP_A, IP_B, PROTO_ICMP, false));
    assert_eq!(payload, &[1, 2, 3, 4]);

    let mut bad = p.clone();
    bad[8] ^= 1;
    assert!(Ipv4::parse(&bad).is_none());
    assert!(Ipv4::parse(&p[..IPV4_HEADER + 3]).is_none());

    let mut fragment = p.clone();
    put16(&mut fragment, 6, 0x2000);
    put16(&mut fragment, 10, 0);
    let c = checksum(sum(&fragment[..IPV4_HEADER], 0));
    put16(&mut fragment, 10, c);
    assert!(Ipv4::parse(&fragment).unwrap().0.is_fragment());
}

#[test]
fn arp_round_trips() {
    let arp = Arp { op: ARP_REQUEST, sha: MAC_A, spa: IP_A, tha: [0; 6], tpa: IP_B };
    let mut p = [0; ARP_LEN];
    arp.write(&mut p);
    assert_eq!(Arp::parse(&p), Some(arp));
    p[5] = 16;
    assert_eq!(Arp::parse(&p), None);
}

#[test]
fn echo_reply_waits_for_arp() {
    let (mut net, a, b) = pair();
    net.run_for(Duration::from_millis(10));
    let start = net.now;
    net.send(a, &echo_request(MAC_B, IP_B, 1));
    net.run_for(Duration::from_millis(10));

    let from_b = frames_from(&net, b);
    assert_eq!(from_b.len(), 2);
    let (dst, request) = arp_of(from_b[0]).unwrap();
    assert_eq!(dst, BROADCAST_MAC);
    assert_eq!((request.op, request.sha, request.spa, request.tpa), (ARP_REQUEST, MAC_B, IP_B, IP_A));

    let (dst, reply) = arp_of(frames_from(&net, a)[1]).unwrap();
    assert_eq!(dst, MAC_B);
    assert_eq!((reply.op, reply.sha, reply.spa, reply.tha, reply.tpa), (ARP_REPLY, MAC_A, IP_A, MAC_B, IP_B));

    let (dst, ip, payload) = icmp_of(from_b[1]).unwrap();
    assert_eq!((dst, ip.src, ip.dst), (MAC_A, IP_B, IP_A));
    assert_eq!(payload, icmp(ICMP_ECHO_REPLY, 0x1234, 1, b"ping data"));
    assert_eq!(net.captured.last().unwrap().at, start + Duration::from_millis(3));
    assert!(from_b.iter().all(|f| f.len() >= 60 && Ethernet::parse(f).unwrap().src == MAC_B));
}

#[test]
fn second_echo_uses_the_cache() {
    let (mut net, a, b) = pair();
    net.send(a, &echo_request(MAC_B, IP_B, 1));
    net.run_for(Duration::from_millis(10));
    let before = net.captured.len();
    net.send(a, &echo_request(MAC_B, IP_B, 2));
    net.run_for(Duration::from_millis(10));

    let new = &net.captured[before..];
    assert_eq!(new.len(), 2);
    assert_eq!(new[1].from, b);
    assert_eq!(new[1].at, ms(11));
    let (_, _, payload) = icmp_of(&new[1].frame).unwrap();
    assert_eq!(payload, icmp(ICMP_ECHO_REPLY, 0x1234, 2, b"ping data"));
}

#[test]
fn unanswered_arp_retries_then_gives_up() {
    let (mut net, _, b) = pair();
    let silent = Ipv4Addr::new(192, 0, 2, 9);
    let sent = net.with(b.0, |stack, now, tx| stack.send_ipv4(None, silent, PROTO_ICMP, 8, now, tx, |_, _| {}));
    assert_eq!(sent, Ok(()));
    net.run_for(Duration::from_secs(10));

    let requests: Vec<Instant> = net
        .captured
        .iter()
        .filter(|c| c.from == b && arp_of(&c.frame).is_some_and(|(_, arp)| arp.tpa == silent))
        .map(|c| c.at)
        .collect();
    assert_eq!(requests, vec![ms(0), ms(1000), ms(2000)]);
    assert_eq!(net.now, ms(10_000));

    let before = net.captured.len();
    net.with(b.0, |stack, now, tx| stack.send_ipv4(None, silent, PROTO_ICMP, 8, now, tx, |_, _| {})).unwrap();
    assert_eq!(net.captured.len(), before + 1);
}

#[test]
fn off_link_goes_through_the_gateway() {
    let (mut net, _, b) = pair();
    net.with(b.0, |stack, now, tx| stack.send_ipv4(None, REMOTE, PROTO_ICMP, 8, now, tx, |_, _| {})).unwrap();
    let (_, arp) = arp_of(&net.captured[0].frame).unwrap();
    assert_eq!(arp.tpa, GATEWAY);

    net.stacks[b.0].set_ipv4(b.1, Some(Ipv4Config { address: IP_B, prefix_len: 24, gateway: None }));
    let sent = net.with(b.0, |stack, now, tx| stack.send_ipv4(None, REMOTE, PROTO_ICMP, 8, now, tx, |_, _| {}));
    assert_eq!(sent, Err(Error::NoRoute));
}

#[test]
fn ignores_what_is_not_for_it() {
    let (mut net, a, b) = pair();
    net.run_for(Duration::from_millis(10));
    let before = net.stacks[b.0].stats(b.1).rx_frames;
    net.send(a, &echo_request(MAC_B, Ipv4Addr::new(192, 0, 2, 3), 1));
    net.send(a, &echo_request(BROADCAST_MAC, Ipv4Addr::BROADCAST, 2));
    net.send(a, &echo_request([0x02, 0, 0, 0, 0, 0x0c], IP_B, 3));

    let mut own = echo_request(BROADCAST_MAC, IP_B, 6);
    own[6..12].copy_from_slice(&MAC_B);
    net.send(a, &own);

    let mut bad = echo_request(MAC_B, IP_B, 4);
    let at = ETHERNET_HEADER + IPV4_HEADER + 8;
    bad[at] ^= 0xff;
    net.send(a, &bad);

    let mut fragment = echo_request(MAC_B, IP_B, 5);
    let ip = &mut fragment[ETHERNET_HEADER..];
    put16(ip, 6, 0x2000);
    put16(ip, 10, 0);
    let c = checksum(sum(&ip[..IPV4_HEADER], 0));
    put16(ip, 10, c);
    net.send(a, &fragment);

    net.run_for(Duration::from_millis(10));
    assert!(frames_from(&net, b).is_empty());
    assert_eq!(net.stacks[b.0].stats(b.1).rx_frames - before, 6);
}

#[test]
fn answers_arp_only_for_its_address() {
    let (mut net, a, b) = pair();
    let ask = |tpa| {
        let mut p = [0; ARP_LEN];
        Arp { op: ARP_REQUEST, sha: MAC_A, spa: IP_A, tha: [0; 6], tpa }.write(&mut p);
        ethernet(BROADCAST_MAC, MAC_A, ETHERTYPE_ARP, &p)
    };
    net.send(a, &ask(Ipv4Addr::new(192, 0, 2, 3)));
    net.send(a, &ask(IP_B));
    net.run_for(Duration::from_millis(10));
    let from_b = frames_from(&net, b);
    assert_eq!(from_b.len(), 1);
    assert_eq!(arp_of(from_b[0]).unwrap().1.op, ARP_REPLY);
}

#[test]
fn loopback_answers_echo() {
    let mut net = Net::new();
    let (s, eth) = host(&mut net, MAC_A, IP_A);
    net.stacks[s].set_link_up(eth, true);
    for dst in [Ipv4Addr::LOCALHOST, IP_A] {
        let request = ipv4(dst, dst, PROTO_ICMP, &icmp(ICMP_ECHO_REQUEST, 7, 1, b"lo"));
        net.with(s, |stack, _, tx| stack.emit(IfaceId::LOOPBACK, &request, tx));
    }
    net.run_for(Duration::from_millis(1));
    let lo = net.stacks[s].stats(IfaceId::LOOPBACK);
    assert_eq!((lo.rx_frames, lo.tx_frames, lo.rx_dropped, lo.tx_dropped), (4, 4, 0, 0));
    assert_eq!(net.stacks[s].stats(eth).tx_frames, 0);
}

#[test]
fn ip_medium_needs_no_arp() {
    let mut net = Net::new();
    let tun = LinkInfo { medium: Medium::Ip, mac: [0; 6], mtu: 1420 };
    let mut ends = Vec::new();
    for ip in [IP_A, IP_B] {
        let mut stack = Stack::new();
        let id = stack.add_interface("tun", tun);
        stack.set_ipv4(id, config(ip));
        ends.push((net.add_stack(stack), id));
    }
    net.connect(ends[0], ends[1], WIRE);
    let request = ipv4(IP_A, IP_B, PROTO_ICMP, &icmp(ICMP_ECHO_REQUEST, 1, 1, b"tun"));
    net.send(ends[0], &request);
    net.run_for(Duration::from_millis(10));

    let from_b = frames_from(&net, ends[1]);
    assert_eq!(from_b.len(), 1);
    let (ip, payload) = Ipv4::parse(from_b[0]).unwrap();
    assert_eq!((ip.src, ip.dst), (IP_B, IP_A));
    assert_eq!(payload, icmp(ICMP_ECHO_REPLY, 1, 1, b"tun"));
}

#[test]
fn too_big_is_refused() {
    let (mut net, _, b) = pair();
    let sent = net.with(b.0, |stack, now, tx| stack.send_ipv4(None, IP_A, PROTO_ICMP, 1481, now, tx, |_, _| {}));
    assert_eq!(sent, Err(Error::MessageTooLarge));
}

fn fragment(src: Ipv4Addr, dst: Ipv4Addr, ident: u16, offset: usize, more: bool, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0; IPV4_HEADER];
    let frag = if more { 0x2000 } else { 0 } | (offset / 8) as u16;
    Ipv4 { src, dst, protocol: PROTO_ICMP, ttl: 64, ident, frag }.write_header(&mut p, payload.len());
    p.extend_from_slice(payload);
    ethernet(MAC_B, MAC_A, ETHERTYPE_IPV4, &p)
}

fn settled() -> (Net, End, End) {
    let (mut net, a, b) = pair();
    net.run_for(Duration::from_millis(10));
    (net, a, b)
}

/// Both ends know each other's MAC, so nothing waits on ARP.
fn acquainted() -> (Net, End, End) {
    let (mut net, a, b) = settled();
    net.send(a, &echo_request(MAC_B, IP_B, 99));
    net.run_for(Duration::from_millis(10));
    (net, a, b)
}

fn icmp_errors_from(net: &Net, from: End) -> Vec<(u8, Vec<u8>)> {
    frames_from(net, from)
        .into_iter()
        .filter_map(|f| icmp_of(f))
        .filter(|(_, _, p)| p[0] == 3)
        .map(|(_, _, p)| (p[1], p))
        .collect()
}

#[test]
fn fragments_are_reassembled_in_any_order() {
    let (mut net, a, b) = settled();
    let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    let request = icmp(ICMP_ECHO_REQUEST, 7, 1, &data);
    net.send(a, &fragment(IP_A, IP_B, 42, 512, false, &request[512..]));
    net.send(a, &fragment(IP_A, IP_B, 42, 0, true, &request[..512]));
    net.run_for(Duration::from_millis(10));
    let replies: Vec<_> = frames_from(&net, b).into_iter().filter_map(icmp_of).collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].2, icmp(ICMP_ECHO_REPLY, 7, 1, &data));
}

#[test]
fn stale_fragments_are_dropped() {
    let (mut net, a, b) = settled();
    let request = icmp(ICMP_ECHO_REQUEST, 7, 1, &[0; 100]);
    net.send(a, &fragment(IP_A, IP_B, 43, 0, true, &request[..64]));
    net.run_for(Duration::from_secs(31));
    net.send(a, &fragment(IP_A, IP_B, 43, 64, false, &request[64..]));
    net.run_for(Duration::from_millis(10));
    assert!(frames_from(&net, b).into_iter().filter_map(icmp_of).next().is_none());
}

#[test]
fn bogus_sources_are_dropped() {
    let (mut net, a, b) = settled();
    for src in [Ipv4Addr::BROADCAST, Ipv4Addr::new(224, 0, 0, 1), Ipv4Addr::LOCALHOST, Ipv4Addr::new(192, 0, 2, 255)] {
        let request = icmp(ICMP_ECHO_REQUEST, 1, 1, b"x");
        net.send(a, &ethernet(MAC_B, MAC_A, ETHERTYPE_IPV4, &ipv4(src, IP_B, PROTO_ICMP, &request)));
    }
    net.run_for(Duration::from_millis(10));
    assert!(frames_from(&net, b).is_empty());
}

fn udp_to(dst: Ipv4Addr, port: u16) -> Vec<u8> {
    let mut udp = vec![0u8; 8];
    put16(&mut udp, 0, 4000);
    put16(&mut udp, 2, port);
    put16(&mut udp, 4, 12);
    udp.extend_from_slice(b"knck");
    let dst_mac = if dst.is_broadcast() { BROADCAST_MAC } else { MAC_B };
    ethernet(dst_mac, MAC_A, ETHERTYPE_IPV4, &ipv4(IP_A, dst, crate::wire::PROTO_UDP, &udp))
}

#[test]
fn closed_ports_and_unknown_protocols_are_reported() {
    let (mut net, a, b) = acquainted();
    net.send(a, &udp_to(IP_B, 9));
    net.send(a, &udp_to(Ipv4Addr::BROADCAST, 9));
    net.send(a, &ethernet(MAC_B, MAC_A, ETHERTYPE_IPV4, &ipv4(IP_A, IP_B, 99, b"data")));
    net.run_for(Duration::from_millis(10));
    let errors = icmp_errors_from(&net, b);
    assert_eq!(errors.iter().map(|(code, _)| *code).collect::<Vec<_>>(), vec![3, 2]);
    let (quoted, rest) = Ipv4::parse_quoted(&errors[0].1[8..]).unwrap();
    assert_eq!((quoted.src, quoted.dst, &rest[..4]), (IP_A, IP_B, &[0x0f, 0xa0, 0, 9][..]));
}

#[test]
fn icmp_errors_are_rate_limited() {
    let (mut net, a, b) = acquainted();
    for _ in 0..20 {
        net.send(a, &udp_to(IP_B, 9));
    }
    net.run_for(Duration::from_millis(10));
    assert_eq!(icmp_errors_from(&net, b).len(), 10);
    net.run_for(Duration::from_secs(1));
    net.send(a, &udp_to(IP_B, 9));
    net.run_for(Duration::from_millis(10));
    assert_eq!(icmp_errors_from(&net, b).len(), 11);
}

#[test]
fn connected_udp_hears_the_refusal_once() {
    let (mut net, a, _) = settled();
    let any = std::net::SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0);
    let s = net.stacks[a.0].udp_bind(any, None, 1024, idf::heap::Caps::DEFAULT).unwrap();
    let peer = std::net::SocketAddrV4::new(IP_B, 9);
    net.stacks[a.0].udp_connect(s, peer);
    net.with(a.0, |stack, now, tx| stack.udp_send(s, peer, b"x", now, tx)).unwrap();
    net.run_for(Duration::from_millis(10));
    let mut buf = [0; 16];
    assert_eq!(net.stacks[a.0].udp_recv(s, &mut buf), Some(Err(Error::ConnectionRefused)));
    assert_eq!(net.stacks[a.0].udp_recv(s, &mut buf), None);
}

/// A connects through B as its gateway; B forwards nothing, so only what the
/// test injects as B answers.
fn routed_through_b() -> (Net, End, End) {
    let (mut net, a, b) = settled();
    net.stacks[a.0].set_ipv4(a.1, Some(Ipv4Config { address: IP_A, prefix_len: 24, gateway: Some(IP_B) }));
    net.run_for(Duration::from_secs(3));
    (net, a, b)
}

fn unreachable_about(net: &Net, a: End, code: u8) -> Vec<u8> {
    let syn = frames_from(net, a)
        .into_iter()
        .rev()
        .find_map(|f| {
            let eth = Ethernet::parse(f)?;
            let (ip, _) = Ipv4::parse(eth.payload)?;
            (ip.protocol == crate::wire::PROTO_TCP).then(|| eth.payload.to_vec())
        })
        .unwrap();
    let mut body = vec![3, code, 0, 0, 0, 0, 0, 0];
    body.extend_from_slice(&syn[..IPV4_HEADER + 8]);
    let c = checksum(sum(&body, 0));
    put16(&mut body, 2, c);
    ethernet(MAC_A, MAC_B, ETHERTYPE_IPV4, &ipv4(IP_B, IP_A, PROTO_ICMP, &body))
}

#[test]
fn icmp_refusal_ends_a_handshake_but_unreachable_does_not() {
    let to = std::net::SocketAddrV4::new(REMOTE, 80);
    let buffers = crate::tcp::Buffers { rx: 1024, tx: 1024, caps: idf::heap::Caps::DEFAULT };

    let (mut net, a, b) = routed_through_b();
    let c = net.with(a.0, |stack, now, tx| stack.tcp_connect(to, buffers, now, tx)).unwrap();
    net.run_for(Duration::from_millis(10));
    let icmp = unreachable_about(&net, a, 1);
    net.send(b, &icmp);
    net.run_for(Duration::from_millis(10));
    assert_eq!(net.stacks[a.0].tcp_connected(c), None);
    let icmp = unreachable_about(&net, a, 3);
    net.send(b, &icmp);
    net.run_for(Duration::from_millis(10));
    assert_eq!(net.stacks[a.0].tcp_connected(c), Some(Err(Error::ConnectionRefused)));
}

#[test]
fn configured_address_is_announced_twice() {
    let (mut net, a, _) = pair();
    net.run_for(Duration::from_secs(5));
    let announced: Vec<Instant> = net
        .captured
        .iter()
        .filter(|c| c.from == a && arp_of(&c.frame).is_some_and(|(dst, arp)| dst == BROADCAST_MAC && arp.spa == IP_A && arp.tpa == IP_A))
        .map(|c| c.at)
        .collect();
    assert_eq!(announced, vec![ms(0), ms(2000)]);
}

#[test]
fn entry_in_use_is_refreshed_before_it_expires() {
    let (mut net, a, b) = settled();
    let any = std::net::SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 9);
    net.stacks[b.0].udp_bind(any, None, 4096, idf::heap::Caps::DEFAULT).unwrap();
    let from = std::net::SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0);
    let s = net.stacks[a.0].udp_bind(from, None, 1024, idf::heap::Caps::DEFAULT).unwrap();
    let to = std::net::SocketAddrV4::new(IP_B, 9);
    for _ in 0..40 {
        net.with(a.0, |stack, now, tx| stack.udp_send(s, to, b"tick", now, tx)).unwrap();
        net.run_for(Duration::from_secs(10));
    }
    let requests: Vec<Mac> = net
        .captured
        .iter()
        .filter(|c| c.from == a)
        .filter_map(|c| arp_of(&c.frame))
        .filter(|(_, arp)| arp.op == ARP_REQUEST && arp.tpa == IP_B)
        .map(|(dst, _)| dst)
        .collect();
    assert_eq!(requests[0], BROADCAST_MAC);
    assert!(requests[1..].iter().all(|&dst| dst == MAC_B), "{requests:02x?}");
    assert!(requests.len() >= 2);
}

fn slow_sum(data: &[u8], acc: u32) -> u32 {
    let mut acc = u64::from(acc);
    let mut pairs = data.chunks_exact(2);
    for p in &mut pairs {
        acc += u64::from(u16::from_be_bytes([p[0], p[1]]));
    }
    if let [last] = pairs.remainder() {
        acc += u64::from(*last) << 8;
    }
    while acc > 0xffff_ffff {
        acc = (acc & 0xffff_ffff) + (acc >> 32);
    }
    acc as u32
}

#[test]
fn fast_checksum_matches_the_plain_one() {
    let data: Vec<u8> = (0..1600u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    for start in 0..8 {
        for len in [0, 1, 2, 3, 7, 20, 41, 64, 1459, 1460, 1500] {
            let slice = &data[start..start + len];
            for acc in [0u32, 0x1234, 0xffff_0000] {
                assert_eq!(checksum(sum(slice, acc)), checksum(slow_sum(slice, acc)), "start {start} len {len} acc {acc:x}");
            }
        }
    }
}
