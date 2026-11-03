/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use idf::heap::Caps;
use idf::thread;

use super::{IP_A, IP_B, Live, MAC_A, MAC_B, config, eth, in_task, v4};
use crate::harness::{End, Net as Wires};
use crate::tcp::{Buffers, State};
use crate::wire::{ETHERTYPE_IPV4, Ethernet, Ipv4, PROTO_TCP};
use crate::{Error, Stack, TcpBuilder};

const ANY: Ipv4Addr = Ipv4Addr::UNSPECIFIED;
const PORT: u16 = 7;

fn buffers(rx: usize, tx: usize) -> Buffers {
    Buffers { rx, tx, caps: Caps::DEFAULT }
}

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

fn listen(net: &mut Wires, end: End, backlog: usize, b: Buffers) -> usize {
    net.stacks[end.0].tcp_listen(SocketAddrV4::new(ANY, PORT), backlog, b).unwrap()
}

fn connect(net: &mut Wires, end: End, to: Ipv4Addr, b: Buffers) -> usize {
    net.with(end.0, |stack, now, tx| stack.tcp_connect(SocketAddrV4::new(to, PORT), b, now, tx)).unwrap()
}

/// A connection from A to a listener on B: (client, server).
fn established(net: &mut Wires, a: End, b: End, client: Buffers, server: Buffers) -> (usize, usize) {
    let l = listen(net, b, 4, server);
    let c = connect(net, a, IP_B, client);
    net.run_for(Duration::from_millis(10));
    assert_eq!(net.stacks[a.0].tcp_connected(c), Some(Ok(())));
    let s = net.stacks[b.0].tcp_accept(l).unwrap();
    (c, s)
}

fn write(net: &mut Wires, end: End, id: usize, data: &[u8]) -> Option<crate::Result<usize>> {
    net.with(end.0, |stack, now, tx| stack.tcp_write(id, data, now, tx))
}

fn read(net: &mut Wires, end: End, id: usize, len: usize) -> Option<crate::Result<Vec<u8>>> {
    let mut buf = vec![0; len];
    let r = net.with(end.0, |stack, now, tx| stack.tcp_read(id, &mut buf, now, tx))?;
    Some(r.map(|n| buf[..n].to_vec()))
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

/// Moves `data` from `from` to `to`, writing and reading as room allows;
/// returns the simulated time it took.
fn transfer(net: &mut Wires, from: (End, usize), to: (End, usize), data: &[u8], limit: Duration) -> Duration {
    let start = net.now;
    let mut sent = 0;
    let mut got = Vec::new();
    while got.len() < data.len() {
        if net.now - start >= limit {
            eprintln!("now={:?}", net.now);
            eprintln!("from: {}", net.stacks[from.0.0].tcp_debug(from.1));
            eprintln!("to:   {}", net.stacks[to.0.0].tcp_debug(to.1));
            panic!("stalled at {} of {} bytes", got.len(), data.len());
        }
        while sent < data.len() {
            match write(net, from.0, from.1, &data[sent..]) {
                Some(Ok(n)) => sent += n,
                Some(Err(e)) => panic!("write failed: {e:?}"),
                None => break,
            }
        }
        while let Some(r) = read(net, to.0, to.1, 4096) {
            let chunk = r.unwrap();
            assert!(!chunk.is_empty(), "early end of stream");
            got.extend_from_slice(&chunk);
        }
        net.run_for(Duration::from_millis(1));
    }
    assert!(got == data, "data differs");
    net.now - start
}

fn segments_from(net: &Wires, from: End) -> Vec<(u8, usize, Vec<u8>)> {
    net.captured
        .iter()
        .filter(|c| c.from == from)
        .filter_map(|c| {
            let eth = Ethernet::parse(&c.frame)?;
            if eth.ethertype != ETHERTYPE_IPV4 {
                return None;
            }
            let (ip, p) = Ipv4::parse(eth.payload)?;
            if ip.protocol != PROTO_TCP {
                return None;
            }
            let header = usize::from(p[12] >> 4) * 4;
            Some((p[13], p.len() - header, p[20..header].to_vec()))
        })
        .collect()
}

#[test]
fn handshake_data_and_close() {
    let (mut net, a, b) = pair();
    let (c, s) = established(&mut net, a, b, buffers(4096, 4096), buffers(4096, 4096));
    assert_eq!(net.stacks[b.0].tcp_addrs(s).1, net.stacks[a.0].tcp_addrs(c).0);

    assert_eq!(write(&mut net, a, c, b"hello"), Some(Ok(5)));
    assert_eq!(read(&mut net, b, s, 64), None);
    net.run_for(Duration::from_millis(10));
    assert_eq!(read(&mut net, b, s, 64), Some(Ok(b"hello".to_vec())));
    assert_eq!(write(&mut net, b, s, b"world"), Some(Ok(5)));
    net.run_for(Duration::from_millis(10));
    assert_eq!(read(&mut net, a, c, 3), Some(Ok(b"wor".to_vec())));
    assert_eq!(read(&mut net, a, c, 64), Some(Ok(b"ld".to_vec())));

    net.with(a.0, |stack, now, tx| stack.tcp_shutdown(c, now, tx));
    net.run_for(Duration::from_millis(10));
    assert_eq!(read(&mut net, b, s, 64), Some(Ok(vec![])));
    assert_eq!(write(&mut net, a, c, b"late"), Some(Err(Error::BrokenPipe)));
    assert_eq!(write(&mut net, b, s, b"still open"), Some(Ok(10)));
    net.run_for(Duration::from_millis(10));
    assert_eq!(read(&mut net, a, c, 64), Some(Ok(b"still open".to_vec())));

    net.with(b.0, |stack, now, tx| stack.tcp_release(s, now, tx));
    net.with(a.0, |stack, now, tx| stack.tcp_release(c, now, tx));
    net.run_for(Duration::from_millis(10));
    assert_eq!(net.stacks[b.0].tcp_state(s), None);
    assert_eq!(net.stacks[a.0].tcp_state(c), Some(State::TimeWait));
    net.run_for(Duration::from_secs(61));
    assert_eq!(net.stacks[a.0].tcp_state(c), None);
}

#[test]
fn syn_carries_mss_and_window_scale() {
    let (mut net, a, b) = pair();
    established(&mut net, a, b, buffers(256 * 1024, 4096), buffers(4096, 4096));
    let syn = &segments_from(&net, a)[0];
    assert_eq!(syn.0, 0x02);
    assert_eq!(syn.2, [2, 4, 0x05, 0xb4, 1, 3, 3, 3]);
    let syn_ack = &segments_from(&net, b)[0];
    assert_eq!(syn_ack.0, 0x12);
    assert_eq!(syn_ack.2, [2, 4, 0x05, 0xb4, 1, 3, 3, 0]);
}

#[test]
fn closed_port_refuses() {
    let (mut net, a, _) = pair();
    let c = connect(&mut net, a, IP_B, buffers(1024, 1024));
    net.run_for(Duration::from_millis(10));
    assert_eq!(net.stacks[a.0].tcp_connected(c), Some(Err(Error::ConnectionRefused)));
}

#[test]
fn silent_peer_times_out() {
    let (mut net, a, _) = pair();
    let c = connect(&mut net, a, Ipv4Addr::new(192, 0, 2, 9), buffers(1024, 1024));
    net.run_for(Duration::from_secs(60));
    assert_eq!(net.stacks[a.0].tcp_connected(c), None);
    net.run_for(Duration::from_secs(120));
    assert_eq!(net.stacks[a.0].tcp_connected(c), Some(Err(Error::TimedOut)));
}

#[test]
fn bulk_transfer_survives_loss() {
    let (mut net, a, b) = pair();
    let (c, s) = established(&mut net, a, b, buffers(16384, 16384), buffers(16384, 16384));
    let mut n = 0;
    net.set_loss(move |frame| {
        n += 1;
        frame.len() > 200 && n % 13 == 0
    });
    let data = pattern(300 * 1024);
    transfer(&mut net, (a, c), (b, s), &data, Duration::from_secs(120));
    let back = pattern(50 * 1024);
    transfer(&mut net, (b, s), (a, c), &back, Duration::from_secs(60));
}

#[test]
fn transfer_survives_random_loss_of_any_frame() {
    for seed in 1..=8u32 {
        let (mut net, a, b) = pair();
        let mut state = seed;
        net.set_loss(move |_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (state >> 16) % 10 == 0
        });
        let l = listen(&mut net, b, 4, buffers(8192, 8192));
        let c = connect(&mut net, a, IP_B, buffers(8192, 8192));
        net.run_for(Duration::from_secs(30));
        assert_eq!(net.stacks[a.0].tcp_connected(c), Some(Ok(())), "seed {seed}");
        let s = net.stacks[b.0].tcp_accept(l).unwrap();
        transfer(&mut net, (a, c), (b, s), &pattern(100 * 1024), Duration::from_secs(300));
        transfer(&mut net, (b, s), (a, c), &pattern(30 * 1024), Duration::from_secs(300));
    }
}

#[test]
fn full_window_waits_for_the_reader() {
    let (mut net, a, b) = pair();
    let (c, s) = established(&mut net, a, b, buffers(4096, 4096), buffers(4096, 4096));
    let data = pattern(4096 + 4096);
    let mut sent = 0;
    for _ in 0..2000 {
        if let Some(r) = write(&mut net, a, c, &data[sent..]) {
            sent += r.unwrap();
        }
        net.run_for(Duration::from_millis(10));
    }
    assert_eq!(sent, data.len());
    assert_eq!(net.stacks[a.0].tcp_state(c), Some(State::Established));
    let mut got = Vec::new();
    while got.len() < data.len() {
        while let Some(r) = read(&mut net, b, s, 1000) {
            got.extend_from_slice(&r.unwrap());
        }
        net.run_for(Duration::from_millis(5));
    }
    assert!(got == data);
}

#[test]
fn releasing_with_unread_data_resets() {
    let (mut net, a, b) = pair();
    let (c, s) = established(&mut net, a, b, buffers(4096, 4096), buffers(4096, 4096));
    write(&mut net, a, c, b"unread");
    net.run_for(Duration::from_millis(10));
    net.with(b.0, |stack, now, tx| stack.tcp_release(s, now, tx));
    net.run_for(Duration::from_millis(10));
    assert_eq!(net.stacks[b.0].tcp_state(s), None);
    assert_eq!(read(&mut net, a, c, 64), Some(Err(Error::ConnectionReset)));
}

#[test]
fn backlog_holds_back_extra_connections() {
    let (mut net, a, b) = pair();
    let l = listen(&mut net, b, 1, buffers(1024, 1024));
    let first = connect(&mut net, a, IP_B, buffers(1024, 1024));
    net.run_for(Duration::from_millis(10));
    let second = connect(&mut net, a, IP_B, buffers(1024, 1024));
    net.run_for(Duration::from_millis(100));
    assert_eq!(net.stacks[a.0].tcp_connected(first), Some(Ok(())));
    assert_eq!(net.stacks[a.0].tcp_connected(second), None);
    assert!(net.stacks[b.0].tcp_accept(l).is_some());
    assert!(net.stacks[b.0].tcp_accept(l).is_none());
    net.run_for(Duration::from_secs(5));
    assert_eq!(net.stacks[a.0].tcp_connected(second), Some(Ok(())));
    assert!(net.stacks[b.0].tcp_accept(l).is_some());
}

#[test]
fn closing_a_listener_resets_its_queue() {
    let (mut net, a, b) = pair();
    let l = listen(&mut net, b, 4, buffers(1024, 1024));
    let c = connect(&mut net, a, IP_B, buffers(1024, 1024));
    net.run_for(Duration::from_millis(10));
    net.with(b.0, |stack, now, tx| stack.tcp_unlisten(l, now, tx));
    net.run_for(Duration::from_millis(10));
    assert_eq!(read(&mut net, a, c, 64), Some(Err(Error::ConnectionReset)));
}

#[test]
fn nagle_holds_small_writes_while_unacked() {
    let (mut net, a, b) = pair();
    let (c, _) = established(&mut net, a, b, buffers(4096, 4096), buffers(4096, 4096));
    let data_segments = |net: &Wires| segments_from(net, a).iter().filter(|s| s.1 > 0).count();
    for _ in 0..5 {
        write(&mut net, a, c, b"x");
    }
    assert_eq!(data_segments(&net), 1);
    net.run_for(Duration::from_millis(100));
    assert_eq!(data_segments(&net), 2);

    net.stacks[a.0].tcp_set_nodelay(c, true);
    for _ in 0..5 {
        write(&mut net, a, c, b"y");
    }
    assert_eq!(data_segments(&net), 7);
}

#[test]
fn large_window_moves_more_per_round_trip() {
    let elapsed = |rx: usize| {
        let (mut net, a, b) = pair();
        let (c, s) = established(&mut net, a, b, buffers(4096, 256 * 1024), buffers(rx, 4096));
        transfer(&mut net, (a, c), (b, s), &pattern(1024 * 1024), Duration::from_secs(60))
    };
    let small = elapsed(4096);
    let large = elapsed(128 * 1024);
    assert!(large * 4 < small, "128 KiB window {large:?}, 4 KiB window {small:?}");
}

#[test]
fn streams_over_live_links() {
    in_task(|| {
        let live = Live::new();
        let listener = TcpBuilder::default().listen_in(live.b, v4(ANY, PORT)).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let (reader, writer) = stream.split();
            let mut buf = [0; 1500];
            loop {
                let n = reader.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n]).unwrap();
            }
        });

        let stream = idf::executor::block_on(TcpBuilder::default().connect_in(live.a, v4(IP_B, PORT))).unwrap();
        let (reader, writer) = stream.split();
        let data = pattern(64 * 1024);
        let sender = {
            let data = data.clone();
            thread::spawn(move || {
                writer.write_all(&data).unwrap();
            })
        };
        let mut got = Vec::new();
        let mut buf = [0; 4096];
        while got.len() < data.len() {
            let n = reader.read_timeout(&mut buf, Duration::from_secs(5)).unwrap();
            assert!(n > 0);
            got.extend_from_slice(&buf[..n]);
        }
        assert!(got == data);
        sender.join();
        assert_eq!(reader.read_timeout(&mut buf, Duration::from_secs(5)), Ok(0));
        server.join();
    });
}

#[test]
fn live_errors_and_timeouts() {
    in_task(|| {
        let live = Live::new();
        let refused = idf::executor::block_on(TcpBuilder::default().connect_in(live.a, v4(IP_B, 1)));
        assert_eq!(refused.err(), Some(Error::ConnectionRefused));

        let silent = idf::executor::block_on(async {
            idf::executor::timeout(
                Duration::from_millis(300),
                TcpBuilder::default().connect_in(live.a, v4(Ipv4Addr::new(192, 0, 2, 9), PORT)),
            )
            .await
        });
        assert!(silent.is_err());

        let listener = TcpBuilder::default().listen_in(live.b, v4(ANY, PORT)).unwrap();
        assert_eq!(listener.accept_timeout(Duration::from_millis(30)).err(), Some(Error::TimedOut));
        let client = idf::executor::block_on(TcpBuilder::default().connect_in(live.a, v4(IP_B, PORT))).unwrap();
        let (server, peer) = listener.accept_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(peer, client.local_addr().unwrap());
        let mut buf = [0; 8];
        assert_eq!(server.read_timeout(&mut buf, Duration::from_millis(30)), Err(Error::TimedOut));
        drop(client);
        assert_eq!(server.read_timeout(&mut buf, Duration::from_secs(1)), Ok(0));
    });
}
