/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::time::Duration;

use idf::heap::Caps;
use idf::{executor, thread};

use super::{IP_A, IP_B, Live, MAC_A, MAC_B, config, eth, in_task, v4};

use crate::harness::{End, Net as Wires};
use crate::stack::SocketId;
use crate::wire::{
    ETHERNET_HEADER, ETHERTYPE_IPV4, Ethernet, IPV4_HEADER, Ipv4, PROTO_UDP, UDP_HEADER, checksum, put16, sum,
};
use crate::{Error, IfaceId, Stack, UdpSocket};

const ANY: Ipv4Addr = Ipv4Addr::UNSPECIFIED;

fn pair() -> (Wires, End, End) {
    let mut net = Wires::new();
    let mut ends = Vec::new();
    for (mac, ip) in [(MAC_A, IP_A), (MAC_B, IP_B)] {
        let mut stack = Stack::new();
        let id = stack.add_interface("eth", eth(mac));
        stack.set_ipv4(id, config(ip));
        ends.push((net.add_stack(stack), id));
    }
    net.connect(ends[0], ends[1], Duration::from_millis(1));
    (net, ends[0], ends[1])
}

fn bind(net: &mut Wires, end: End, ip: Ipv4Addr, port: u16, size: usize) -> usize {
    net.stacks[end.0].udp_bind(SocketAddrV4::new(ip, port), None, size, Caps::DEFAULT).unwrap()
}

fn send(net: &mut Wires, end: End, id: usize, to: SocketAddrV4, data: &[u8]) -> crate::Result<()> {
    net.with(end.0, |stack, now, tx| stack.udp_send(id, to, data, now, tx))
}

fn recv(net: &mut Wires, end: End, id: usize, len: usize) -> Option<(Vec<u8>, SocketAddrV4)> {
    let mut buf = vec![0; len];
    let (n, from) = net.stacks[end.0].udp_recv(id, &mut buf)?.unwrap();
    buf.truncate(n);
    Some((buf, from))
}

/// A UDP frame from A to B, written by hand.
fn raw_udp(dst_ip: Ipv4Addr, sport: u16, dport: u16, data: &[u8], checksum_on: bool) -> Vec<u8> {
    let len = UDP_HEADER + data.len();
    let mut udp = vec![0; UDP_HEADER];
    put16(&mut udp, 0, sport);
    put16(&mut udp, 2, dport);
    put16(&mut udp, 4, len as u16);
    udp.extend_from_slice(data);
    if checksum_on {
        let acc = sum(&IP_A.octets(), sum(&dst_ip.octets(), 0)) + u32::from(PROTO_UDP) + len as u32;
        let c = checksum(sum(&udp, acc));
        put16(&mut udp, 6, c);
    }
    let mut frame = vec![0; ETHERNET_HEADER + IPV4_HEADER];
    let dst_mac = if dst_ip.is_broadcast() { [0xff; 6] } else { MAC_B };
    Ethernet::write_header(&mut frame, dst_mac, MAC_A, ETHERTYPE_IPV4);
    Ipv4 { src: IP_A, dst: dst_ip, protocol: PROTO_UDP, ttl: 64, ident: 9, frag: 0 }
        .write_header(&mut frame[ETHERNET_HEADER..], len);
    frame.extend_from_slice(&udp);
    frame
}

#[test]
fn datagrams_cross_both_ways() {
    let (mut net, a, b) = pair();
    let sa = bind(&mut net, a, ANY, 0, 1024);
    let sb = bind(&mut net, b, IP_B, 7, 1024);
    let local_a = net.stacks[a.0].udp_local(sa);
    assert!(local_a.port() >= 49152);

    send(&mut net, a, sa, SocketAddrV4::new(IP_B, 7), b"hello").unwrap();
    net.run_for(Duration::from_millis(10));
    assert!(net.stacks[b.0].woken.contains(&SocketId::Udp(sb)));
    let (data, from) = recv(&mut net, b, sb, 64).unwrap();
    assert_eq!((&data[..], from), (&b"hello"[..], SocketAddrV4::new(IP_A, local_a.port())));
    assert!(recv(&mut net, b, sb, 64).is_none());

    send(&mut net, b, sb, from, b"world").unwrap();
    net.run_for(Duration::from_millis(10));
    let (data, from) = recv(&mut net, a, sa, 64).unwrap();
    assert_eq!((&data[..], from), (&b"world"[..], SocketAddrV4::new(IP_B, 7)));

    let frame = &net.captured.iter().rev().find(|c| c.from == b).unwrap().frame;
    let (ip, udp) = Ipv4::parse(&frame[ETHERNET_HEADER..]).unwrap();
    let acc = sum(&ip.src.octets(), sum(&ip.dst.octets(), 0)) + u32::from(PROTO_UDP) + udp.len() as u32;
    assert_eq!(checksum(sum(udp, acc)), 0);
}

#[test]
fn checksums_are_checked_unless_zero() {
    let (mut net, a, b) = pair();
    let sb = bind(&mut net, b, ANY, 9, 1024);
    let mut bad = raw_udp(IP_B, 1000, 9, b"bad", true);
    *bad.last_mut().unwrap() ^= 1;
    net.send(a, &bad);
    net.send(a, &raw_udp(IP_B, 1000, 9, b"none", false));
    net.send(a, &raw_udp(IP_B, 1000, 9, b"good", true));
    net.run_for(Duration::from_millis(10));
    assert_eq!(recv(&mut net, b, sb, 64).unwrap().0, b"none");
    assert_eq!(recv(&mut net, b, sb, 64).unwrap().0, b"good");
    assert!(recv(&mut net, b, sb, 64).is_none());
}

#[test]
fn full_buffer_drops_and_short_reads_truncate() {
    let (mut net, a, b) = pair();
    let sb = bind(&mut net, b, ANY, 9, 16);
    for data in [&b"0123456789"[..], b"abcdefghij", b"xyz", b""] {
        net.send(a, &raw_udp(IP_B, 1000, 9, data, true));
    }
    net.run_for(Duration::from_millis(10));
    assert_eq!(recv(&mut net, b, sb, 4).unwrap().0, b"0123");
    assert_eq!(recv(&mut net, b, sb, 64).unwrap().0, b"xyz");
    assert_eq!(recv(&mut net, b, sb, 64).unwrap().0, b"");
    assert!(recv(&mut net, b, sb, 64).is_none());

    for data in [&b"wrapping!!"[..], b"around"] {
        net.send(a, &raw_udp(IP_B, 1000, 9, data, true));
    }
    net.run_for(Duration::from_millis(10));
    assert_eq!(recv(&mut net, b, sb, 64).unwrap().0, b"wrapping!!");
    assert_eq!(recv(&mut net, b, sb, 64).unwrap().0, b"around");
}

#[test]
fn connected_sockets_filter_the_peer() {
    let (mut net, a, b) = pair();
    let sb = bind(&mut net, b, ANY, 9, 1024);
    net.stacks[b.0].udp_connect(sb, SocketAddrV4::new(IP_A, 1000));
    net.send(a, &raw_udp(IP_B, 2000, 9, b"other", true));
    net.send(a, &raw_udp(IP_B, 1000, 9, b"peer", true));
    net.run_for(Duration::from_millis(10));
    assert_eq!(recv(&mut net, b, sb, 64).unwrap().0, b"peer");
    assert!(recv(&mut net, b, sb, 64).is_none());
}

#[test]
fn broadcast_reaches_unbound_address() {
    let (mut net, a, b) = pair();
    let sb = bind(&mut net, b, ANY, 68, 1024);
    let sb_specific = bind(&mut net, b, IP_B, 69, 1024);
    net.send(a, &raw_udp(Ipv4Addr::BROADCAST, 67, 68, b"offer", true));
    net.send(a, &raw_udp(Ipv4Addr::BROADCAST, 67, 69, b"miss", true));
    net.run_for(Duration::from_millis(10));
    assert_eq!(recv(&mut net, b, sb, 64).unwrap().0, b"offer");
    assert!(recv(&mut net, b, sb_specific, 64).is_none());
}

#[test]
fn binding_rules() {
    let (mut net, _, b) = pair();
    let stack = &mut net.stacks[b.0];
    let bind = |stack: &mut Stack, ip, port| stack.udp_bind(SocketAddrV4::new(ip, port), None, 64, Caps::DEFAULT);
    let s7 = bind(stack, IP_B, 7).unwrap();
    assert_eq!(bind(stack, IP_B, 7), Err(Error::AddrInUse));
    assert_eq!(bind(stack, ANY, 7), Err(Error::AddrInUse));
    assert!(bind(stack, Ipv4Addr::LOCALHOST, 8).is_ok());
    assert_eq!(bind(stack, Ipv4Addr::new(192, 0, 2, 9), 8), Err(Error::AddrNotAvailable));
    let e1 = bind(stack, ANY, 0).unwrap();
    let e2 = bind(stack, ANY, 0).unwrap();
    assert_ne!(stack.udp_local(e1).port(), stack.udp_local(e2).port());
    stack.udp_close(s7);
    assert!(bind(stack, IP_B, 7).is_ok());
}

#[test]
fn oversized_datagrams_are_refused() {
    let (mut net, a, _) = pair();
    let sa = bind(&mut net, a, ANY, 0, 64);
    let to = SocketAddrV4::new(IP_B, 7);
    assert_eq!(send(&mut net, a, sa, to, &[0; 1473]), Err(Error::MessageTooLarge));
    assert_eq!(send(&mut net, a, sa, to, &[0; 1472]), Ok(()));
    assert_eq!(send(&mut net, a, sa, SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 1), 7), b"x"), Err(Error::NoRoute));
}

#[test]
fn loopback_datagram() {
    let mut net = Wires::new();
    let s = net.add_stack(Stack::new());
    let lo = (s, IfaceId::LOOPBACK);
    let rx = bind(&mut net, lo, Ipv4Addr::LOCALHOST, 5353, 64);
    let tx = bind(&mut net, lo, ANY, 0, 64);
    send(&mut net, lo, tx, SocketAddrV4::new(Ipv4Addr::LOCALHOST, 5353), b"self").unwrap();
    net.run_for(Duration::from_millis(1));
    let (data, from) = recv(&mut net, lo, rx, 64).unwrap();
    assert_eq!(data, b"self");
    assert_eq!(*from.ip(), Ipv4Addr::LOCALHOST);
}

#[test]
fn blocking_and_async_receive() {
    in_task(blocking_and_async_receive_in_task);
}

fn blocking_and_async_receive_in_task() {
    let live = Live::new();
    let server = UdpSocket::builder().bind_in(live.b, v4(ANY, 7)).unwrap();
    let client = UdpSocket::builder().bind_in(live.a, v4(ANY, 0)).unwrap();
    client.connect(v4(IP_B, 7)).unwrap();
    assert_eq!(client.peer_addr(), Ok(v4(IP_B, 7)));

    client.send(b"blocking").unwrap();
    let mut buf = [0; 32];
    let (n, from) = server.recv_from_timeout(&mut buf, Duration::from_secs(1)).unwrap();
    assert_eq!(&buf[..n], b"blocking");
    assert_eq!(from, v4(IP_A, client.local_addr().unwrap().port()));

    let sender = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        client.send(b"async").unwrap();
        client
    });
    let n = executor::block_on(server.recv_async(&mut buf)).unwrap();
    assert_eq!(&buf[..n], b"async");
    let client = sender.join();

    server.send_to(b"back", from).unwrap();
    assert_eq!(client.recv(&mut buf), Ok(4));
}

#[test]
fn receive_times_out() {
    in_task(receive_times_out_in_task);
}

fn receive_times_out_in_task() {
    let live = Live::new();
    let socket = UdpSocket::builder().bind_in(live.a, v4(ANY, 9)).unwrap();
    let start = idf::time::Instant::now();
    let mut buf = [0; 8];
    assert_eq!(socket.recv_from_timeout(&mut buf, Duration::from_millis(30)), Err(Error::TimedOut));
    assert!(start.elapsed() >= Duration::from_millis(30));
    drop(socket);
    assert!(UdpSocket::builder().bind_in(live.a, v4(ANY, 9)).is_ok());
}

#[test]
fn api_refuses_v6_and_needs_a_started_stack() {
    let live = Live::new();
    let v6 = SocketAddr::V6(SocketAddrV6::new(std::net::Ipv6Addr::LOCALHOST, 7, 0, 0));
    assert_eq!(UdpSocket::builder().bind_in(live.a, v6).err(), Some(Error::Unsupported));
    assert_eq!(UdpSocket::bind(v4(ANY, 7)).err(), Some(Error::NotStarted));
}

