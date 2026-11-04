/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The peer of firmware/esp_hosted_enhanced/development/netbench.py: the same
 * ports and protocol as the lwIP netbench.cpp there.
 */

use alloc::vec;
use core::ffi::CStr;
use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use core::time::Duration;

use devkit_net::{TcpBuilder, TcpListener, TcpStream, UdpBuilder, UdpSocket};
use idf::executor::{self, Spawner};
use idf::log::{error, info};
use idf::time::Instant;

const TAG: &CStr = c"netbench";

const SINK_PORT: u16 = 5001;
const SOURCE_PORT: u16 = 5002;
const ECHO_PORT: u16 = 5003;
const UDP_PORT: u16 = 5004;

const BUF: usize = 16 * 1024;
/// As CONFIG_LWIP_TCP_WND_DEFAULT and _SND_BUF_DEFAULT of the lwIP build, on
/// the side that carries the stream; the other side stays small so a board
/// without PSRAM fits a connection.
const TCP_BUFFER: usize = 65534;
const TCP_IDLE_SIDE: usize = 4096;
const UDP_BUFFER: usize = 16 * 1024;

const UDP_DATA: u8 = 0;
const UDP_RESET: u8 = 1;
const UDP_REPORT: u8 = 2;
const UDP_SEND: u8 = 3;
const HDR: usize = 16;

fn any(port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
}

fn listen(port: u16, sink_side: bool) -> Option<TcpListener> {
    let (rx, tx) = if sink_side { (TCP_BUFFER, TCP_IDLE_SIDE) } else { (TCP_IDLE_SIDE, TCP_BUFFER) };
    TcpBuilder::default()
        .rx_buffer(rx)
        .tx_buffer(tx)
        .listen(any(port))
        .inspect_err(|err| error!(TAG, "listen {port}: {err}"))
        .ok()
}

async fn sink(stream: TcpStream) {
    let mut buf = vec![0u8; BUF];
    let mut total = 0usize;
    while let Ok(n) = stream.read_async(&mut buf).await {
        if n == 0 {
            break;
        }
        total += n;
    }
    info!(TAG, "sink: {total} bytes");
}

async fn source(stream: TcpStream) {
    let buf: alloc::vec::Vec<u8> = (0..BUF).map(|i| i as u8).collect();
    let mut total = 0usize;
    while stream.write_all_async(&buf).await.is_ok() {
        total += buf.len();
    }
    info!(TAG, "source: {total} bytes");
}

async fn serve(spawner: Spawner, port: u16, sink_side: bool) {
    let Some(listener) = listen(port, sink_side) else { return };
    loop {
        match listener.accept_async().await {
            Ok((stream, _)) if sink_side => drop(spawner.spawn(sink(stream))),
            Ok((stream, _)) => drop(spawner.spawn(source(stream))),
            Err(err) => error!(TAG, "accept {port}: {err}"),
        }
    }
}

async fn echo() {
    let Ok(socket) = UdpBuilder::default().rx_buffer(UDP_BUFFER).bind(any(ECHO_PORT)) else { return };
    let mut buf = vec![0u8; BUF];
    loop {
        if let Ok((n, from)) = socket.recv_from_async(&mut buf).await {
            let _ = socket.send_to(&buf[..n], from);
        }
    }
}

fn header(cmd: u8, args: [u32; 3]) -> [u8; HDR] {
    let mut h = [0u8; HDR];
    h[0] = cmd;
    for (i, a) in args.iter().enumerate() {
        h[4 + i * 4..8 + i * 4].copy_from_slice(&a.to_le_bytes());
    }
    h
}

fn arg(h: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(h[4 + i * 4..8 + i * 4].try_into().unwrap())
}

/// args: duration (ms), datagram size, rate (kbit/s, 0 for unpaced).
async fn udp_send(socket: &UdpSocket, peer: SocketAddr, req: &[u8]) {
    let size = (arg(req, 1) as usize).clamp(HDR, 1472);
    let rate = u64::from(arg(req, 2));
    let mut buf = vec![0u8; size];
    buf[0] = UDP_DATA;
    let start = Instant::now();
    let end = start + Duration::from_millis(u64::from(arg(req, 0)));
    let (mut sent, mut errors) = (0u32, 0u32);
    while Instant::now() < end {
        if rate > 0 {
            let due = start + Duration::from_micros(u64::from(sent) * size as u64 * 8000 / rate);
            if let Some(wait) = due.checked_duration_since(Instant::now())
                && wait > Duration::from_millis(2)
            {
                executor::sleep(wait).await;
                continue;
            }
        }
        buf[4..8].copy_from_slice(&sent.to_le_bytes());
        match socket.send_to_async(&buf, peer).await {
            Ok(_) => sent += 1,
            Err(_) => errors += 1,
        }
    }
    let elapsed = start.elapsed().as_micros() as u32;
    for _ in 0..5 {
        executor::sleep(Duration::from_millis(50)).await;
        let _ = socket.send_to(&header(UDP_REPORT, [sent, errors, elapsed]), peer);
    }
}

async fn udp() {
    let Ok(socket) = UdpBuilder::default().rx_buffer(UDP_BUFFER).bind(any(UDP_PORT)) else { return };
    let mut buf = vec![0u8; BUF];
    let (mut packets, mut bytes) = (0u32, 0u32);
    let (mut first, mut last) = (Instant::now(), Instant::now());
    loop {
        let Ok((n, peer)) = socket.recv_from_async(&mut buf).await else { continue };
        if n < HDR {
            continue;
        }
        match buf[0] {
            UDP_DATA => {
                last = Instant::now();
                if packets == 0 {
                    first = last;
                }
                packets += 1;
                bytes += n as u32;
            }
            UDP_RESET => (packets, bytes) = (0, 0),
            UDP_REPORT => {
                let span = last.duration_since(first).as_micros() as u32;
                let _ = socket.send_to(&header(UDP_REPORT, [packets, bytes, span]), peer);
            }
            UDP_SEND => {
                let req = buf[..HDR].to_vec();
                udp_send(&socket, peer, &req).await;
            }
            _ => {}
        }
    }
}

pub fn start(spawner: &Spawner) {
    drop(spawner.spawn(serve(spawner.clone(), SINK_PORT, true)));
    drop(spawner.spawn(serve(spawner.clone(), SOURCE_PORT, false)));
    drop(spawner.spawn(echo()));
    drop(spawner.spawn(udp()));
}
