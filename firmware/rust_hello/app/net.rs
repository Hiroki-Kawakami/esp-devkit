/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
use alloc::format;
#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
use alloc::string::String;
use core::ffi::CStr;
use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use devkit_net::{TcpListener, TcpStream, UdpSocket};
#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
use idf::executor;
use idf::executor::Spawner;
use idf::log::{error, info};

const TAG: &CStr = c"rust_hello_net";
const ECHO_PORT: u16 = 7;

fn any(port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
}

async fn echo_tcp(stream: TcpStream) {
    let mut buf = [0u8; 1460];
    loop {
        match stream.read_async(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if stream.write_all_async(&buf[..n]).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn echo_udp(socket: UdpSocket) {
    let mut buf = [0u8; 1472];
    loop {
        if let Ok((n, from)) = socket.recv_from_async(&mut buf).await {
            let _ = socket.send_to(&buf[..n], from);
        }
    }
}

/// TCP and UDP echo on port 7 of every interface.
pub async fn serve_echo(spawner: &Spawner) {
    let udp = match UdpSocket::bind(any(ECHO_PORT)) {
        Ok(socket) => socket,
        Err(err) => {
            error!(TAG, "udp echo: {err}");
            return;
        }
    };
    let listener = match TcpListener::bind(any(ECHO_PORT)) {
        Ok(listener) => listener,
        Err(err) => {
            error!(TAG, "tcp echo: {err}");
            return;
        }
    };
    spawner.spawn(echo_udp(udp));
    info!(TAG, "echo on port {ECHO_PORT}");
    loop {
        match listener.accept_async().await {
            Ok((stream, peer)) => {
                info!(TAG, "tcp echo from {peer}");
                spawner.spawn(echo_tcp(stream));
            }
            Err(err) => error!(TAG, "accept: {err}"),
        }
    }
}

#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
fn wifi_command(args: &[&str]) -> Result<Option<String>, String> {
    let wifi = devkit_net::wifi().ok_or("no wifi")?;
    match args {
        ["join", ssid, rest @ ..] => {
            let password = rest.first().copied().unwrap_or("");
            executor::block_on(wifi.connect(&devkit_net::StaConfig::new(ssid, password)))
                .map_err(|err| format!("{err:?}"))?;
            let ip = wifi
                .sta()
                .wait_ipv4_timeout(core::time::Duration::from_secs(15))
                .map_err(|_| String::from("joined, no address"))?;
            Ok(Some(format!("{}/{} gw {:?}", ip.address, ip.prefix_len, ip.gateway)))
        }
        ["leave"] => {
            executor::block_on(wifi.disconnect()).map_err(|err| format!("{err:?}"))?;
            Ok(None)
        }
        [] => Ok(Some(format!("{:?}", wifi.state()))),
        _ => Err(String::from("usage: wifi [join <ssid> [password] | leave]")),
    }
}

#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
fn net_command(_: &[&str]) -> Result<Option<String>, String> {
    let mut out = String::new();
    for iface in devkit_net::interfaces() {
        let stats = iface.stats();
        out += &format!(
            "{} up={} mac={:02x?} ipv4={:?} dns={:?} rx={}/{} tx={}/{}; ",
            iface.name(),
            iface.is_link_up(),
            iface.link_info().mac,
            iface.ipv4(),
            iface.dns_servers(),
            stats.rx_frames,
            stats.rx_dropped,
            stats.tx_frames,
            stats.tx_dropped
        );
    }
    Ok(Some(out))
}

#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
pub fn register_commands() {
    harness_rs::register("wifi", wifi_command);
    harness_rs::register("net", net_command);
}
