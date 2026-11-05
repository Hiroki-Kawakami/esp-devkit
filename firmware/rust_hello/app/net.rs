/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::format;
use alloc::string::String;
use alloc::vec;
use core::ffi::CStr;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use core::time::Duration;

use devkit_net::wifi_manager::{self, Manager};
use devkit_net::{TcpListener, TcpStream, UdpSocket};
use idf::executor::{self, Spawner};
use idf::log::{error, info};
use idf::nvs::{Key, Nvs};
use idf::heap::{self, Caps};

const TAG: &CStr = c"rust_hello_net";

pub static INT_FREE_AT_MAIN: AtomicUsize = AtomicUsize::new(0);
const ECHO_PORT: u16 = 7;
const REPLY_WAIT: Duration = Duration::from_secs(5);
fn any(port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
}

async fn echo_tcp(stream: TcpStream) {
    let mut buf = vec![0u8; 1460];
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
    let mut buf = vec![0u8; 1472];
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

const OLD_NAMESPACE: &CStr = c"rust_hello";
const OLD_SSID: Key<String> = Key::new(c"ssid");
const OLD_PASSWORD: Key<String> = Key::new(c"password");

/// Moves the network an older rust_hello saved into the manager.
pub fn import_old_network() {
    let Some(m) = wifi_manager::get() else { return };
    let result = Nvs::open(OLD_NAMESPACE).and_then(|mut nvs| {
        let Some(ssid) = nvs.get(OLD_SSID)? else { return Ok(false) };
        let password = nvs.get(OLD_PASSWORD)?.unwrap_or_default();
        m.add(ssid.as_bytes(), &password, false)?;
        nvs.erase_all()?;
        nvs.commit()?;
        Ok(true)
    });
    match result {
        Ok(true) => info!(TAG, "imported the old saved network"),
        Ok(false) => {}
        Err(err) => error!(TAG, "old saved network: {err}"),
    }
}

fn ssid(bytes: &[u8]) -> alloc::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

fn manager() -> Result<&'static Manager, String> {
    wifi_manager::get().ok_or_else(|| String::from("wifi manager not started"))
}

/// Logs what a scan watch reports, `rounds` times.
async fn watch_scans(manager: &'static Manager, interval: Duration, rounds: u32) {
    let mut watch = manager.watch_scan(interval);
    for round in 0..rounds {
        match watch.changed().await {
            Ok(list) => info!(TAG, "watch {round}: {} networks", list.len()),
            Err(err) => info!(TAG, "watch {round}: {err:?}"),
        }
    }
    info!(TAG, "watch done");
}

/// Logs every state the manager goes through.
pub async fn log_wifi_states() {
    let Some(manager) = wifi_manager::get() else { return };
    loop {
        info!(TAG, "wifi state {:?}", manager.changed().await);
    }
}

fn wifi_command(args: &[&str]) -> Result<Option<String>, String> {
    let m = manager()?;
    let err = |e: &dyn core::fmt::Debug| format!("{e:?}");
    match args {
        [] => Ok(Some(format!("{} {:?}", if m.enabled() { "on" } else { "off" }, m.state()))),
        ["on"] => executor::block_on(m.set_enabled(true)).map(|()| None).map_err(|e| err(&e)),
        ["off"] => executor::block_on(m.set_enabled(false)).map(|()| None).map_err(|e| err(&e)),
        ["scan"] => {
            let list = executor::block_on(m.scan()).map_err(|e| err(&e))?;
            let mut out = String::new();
            for e in list {
                out += &format!(
                    "{} {} ch{} {:?}{}{}; ",
                    ssid(&e.ssid),
                    e.rssi,
                    e.channel,
                    e.security,
                    if e.saved { " saved" } else { "" },
                    if e.connected { " connected" } else { "" }
                );
            }
            Ok(Some(out))
        }
        ["watch", interval, rounds] => {
            let interval = Duration::from_secs(interval.parse().map_err(|_| "bad interval")?);
            let rounds = rounds.parse().map_err(|_| "bad rounds")?;
            idf::thread::Builder::new()
                .name("watch_scans")
                .stack_size(6144)
                .spawn(move || executor::block_on(watch_scans(m, interval, rounds)))
                .map_err(|e| err(&e))?;
            Ok(None)
        }
        ["join", name, rest @ ..] => {
            let hidden = rest.last() == Some(&"hidden");
            let password = rest.first().filter(|p| **p != "hidden").copied().unwrap_or("");
            executor::block_on(m.join(name.as_bytes(), password, hidden)).map_err(|e| err(&e))?;
            let ip = devkit_net::wifi()
                .ok_or("no wifi")?
                .sta()
                .wait_ipv4_timeout(Duration::from_secs(15))
                .map_err(|_| String::from("joined, no address"))?;
            Ok(Some(format!("{}/{} gw {:?}", ip.address, ip.prefix_len, ip.gateway)))
        }
        ["rejoin", name] => executor::block_on(m.join_saved(name.as_bytes())).map(|()| None).map_err(|e| err(&e)),
        ["add", name, rest @ ..] => {
            let hidden = rest.last() == Some(&"hidden");
            let password = rest.first().filter(|p| **p != "hidden").copied().unwrap_or("");
            m.add(name.as_bytes(), password, hidden).map(|()| None).map_err(|e| err(&e))
        }
        ["saved"] => {
            let mut out = String::new();
            for n in m.networks() {
                out += &format!(
                    "{}{}{}; ",
                    ssid(&n.ssid),
                    if n.secured { " secured" } else { " open" },
                    if n.hidden { " hidden" } else { "" }
                );
            }
            Ok(Some(out))
        }
        ["forget", name] => executor::block_on(m.forget(name.as_bytes())).map(|f| Some(format!("{f}"))).map_err(|e| err(&e)),
        ["leave"] => {
            executor::block_on(m.disconnect());
            Ok(None)
        }
        ["reconnect"] => {
            m.reconnect();
            Ok(None)
        }
        ["ps", mode] => {
            let mode = match *mode {
                "none" => devkit_net::PowerSave::None,
                "min" | "default" => devkit_net::PowerSave::Min,
                "max" => devkit_net::PowerSave::Max,
                _ => return Err(String::from("usage: wifi ps none|min|max")),
            };
            let wifi = devkit_net::wifi().ok_or("no wifi")?;
            executor::block_on(wifi.set_power_save(mode)).map_err(|e| err(&e))?;
            Ok(None)
        }
        _ => Err(String::from(
            "usage: wifi [on | off | scan | watch <s> <n> | join <ssid> [password] [hidden] | rejoin <ssid> | \
             add <ssid> [password] [hidden] | saved | forget <ssid> | leave | reconnect | ps none|min|max]",
        )),
    }
}

fn net_command(_: &[&str]) -> Result<Option<String>, String> {
    let mut out = String::new();
    for iface in devkit_net::interfaces() {
        let stats = iface.stats();
        out += &format!(
            "{} up={} mac={:02x?} ipv4={:?} dns={:?} rx={}/{}/{} tx={}/{}; ",
            iface.name(),
            iface.is_link_up(),
            iface.link_info().mac,
            iface.ipv4(),
            iface.dns_servers(),
            stats.rx_frames,
            stats.rx_dropped,
            stats.rx_overflow,
            stats.tx_frames,
            stats.tx_dropped
        );
    }
    Ok(Some(out))
}

/// Sends `text` and returns what comes back first.
fn exchange_command(args: &[&str], udp: bool) -> Result<Option<String>, String> {
    let [host, port, text] = args else {
        return Err(String::from("usage: <host> <port> <text>"));
    };
    let port: u16 = port.parse().map_err(|_| "bad port")?;
    let to = (*host, port);
    let mut buf = vec![0u8; 1472];
    let n = if udp {
        let socket = UdpSocket::bind(any(0)).map_err(|err| format!("{err}"))?;
        socket.connect_timeout(to, REPLY_WAIT).map_err(|err| format!("{err}"))?;
        socket.send(text.as_bytes()).map_err(|err| format!("{err}"))?;
        socket.recv_timeout(&mut buf, REPLY_WAIT).map_err(|err| format!("{err}"))?
    } else {
        let stream = TcpStream::connect_timeout(to, REPLY_WAIT).map_err(|err| format!("{err}"))?;
        stream.write_all(text.as_bytes()).map_err(|err| format!("{err}"))?;
        stream.read_timeout(&mut buf, REPLY_WAIT).map_err(|err| format!("{err}"))?
    };
    Ok(Some(String::from_utf8_lossy(&buf[..n]).into_owned()))
}

fn dns_command(args: &[&str]) -> Result<Option<String>, String> {
    let [host] = args else {
        return Err(String::from("usage: <host>"));
    };
    let addrs = devkit_net::dns::lookup_timeout(host, REPLY_WAIT).map_err(|err| format!("{err}"))?;
    Ok(Some(addrs.iter().map(|a| format!("{a}")).collect::<vec::Vec<_>>().join(" ")))
}

/// As the lwIP netbench firmware prints them, for netbench.py.
fn heap_command(_: &[&str]) -> Result<Option<String>, String> {
    Ok(Some(format!(
        "{} {} {} {} {} {}",
        heap::free_size(Caps::INTERNAL),
        heap::minimum_free_size(Caps::INTERNAL),
        heap::largest_free_block(Caps::INTERNAL),
        heap::free_size(Caps::INTERNAL | Caps::DMA),
        heap::free_size(Caps::SPIRAM),
        INT_FREE_AT_MAIN.load(Ordering::Relaxed)
    )))
}

/// Sends `count` datagrams of `size` bytes back to back; the average time of
/// one send in µs.
fn flood_command(args: &[&str]) -> Result<Option<String>, String> {
    let [ip, port, count, size] = args else {
        return Err(String::from("usage: <ip> <port> <count> <size>"));
    };
    let to = SocketAddr::V4(SocketAddrV4::new(
        ip.parse().map_err(|_| "bad address")?,
        port.parse().map_err(|_| "bad port")?,
    ));
    let count: u64 = count.parse().map_err(|_| "bad count")?;
    let size: usize = size.parse().map_err(|_| "bad size")?;
    let socket = UdpSocket::bind(any(0)).map_err(|err| format!("{err}"))?;
    let data = vec![0x5au8; size];
    let start = idf::time::Instant::now();
    let mut failed = 0;
    for _ in 0..count {
        if socket.send_to(&data, to).is_err() {
            failed += 1;
        }
    }
    let total = start.elapsed().as_micros() as u64;
    Ok(Some(format!("send {:.1} us; failed {failed}", total as f64 / count as f64)))
}

/// Every interface's counters.
fn diag_command(_: &[&str]) -> Result<Option<String>, String> {
    let mut out = String::new();
    for iface in devkit_net::interfaces() {
        out += &format!("{} {:?}; ", iface.name(), iface.stats());
    }
    Ok(Some(out))
}

#[cfg(CONFIG_SOC_WIFI_SUPPORTED)]
fn wifistat_command(_: &[&str]) -> Result<Option<String>, String> {
    unsafe extern "C" {
        fn esp_wifi_statis_dump(modules: u32) -> i32;
    }
    unsafe { esp_wifi_statis_dump(u32::MAX) };
    Ok(None)
}

/// TCP over loopback to the netbench sink, logged when done.
fn cpu_command(_: &[&str]) -> Result<Option<String>, String> {
    drop(idf::thread::Builder::new().stack_size(8192).priority(5).spawn(loopback_bench));
    Ok(None)
}

fn loopback_bench() {
    let to = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 5001));
    let Ok(stream) = devkit_net::TcpBuilder::default().rx_buffer(1024).tx_buffer(8192).connect(to) else { return };
    let buf = vec![0x5au8; 2048];
    let start = idf::time::Instant::now();
    for _ in 0..256 {
        if stream.write_all(&buf).is_err() {
            return;
        }
    }
    let us = start.elapsed().as_micros() as f64;
    info!(TAG, "loopback 512 KiB {:.1} Mbps", 512.0 * 1024.0 * 8.0 / us);
}

#[cfg(target_arch = "xtensa")]
mod perf {
    unsafe extern "C" {
        fn xtensa_perfmon_init(id: i32, select: u16, mask: u16, kernelcnt: i32, tracelevel: i32) -> i32;
        fn xtensa_perfmon_start();
        fn xtensa_perfmon_stop();
        fn xtensa_perfmon_value(id: i32) -> u32;
    }

    /// With four numbers: counter 0 and 1 select and mask, started on core 1.
    /// Without: both counters, read and stopped.
    pub fn command(args: &[&str]) -> Result<Option<alloc::string::String>, alloc::string::String> {
        let nums: alloc::vec::Vec<u16> = args.iter().filter_map(|a| u16::from_str_radix(a.trim_start_matches("0x"), if a.starts_with("0x") { 16 } else { 10 }).ok()).collect();
        let handle = idf::thread::Builder::new()
            .core(Some(1))
            .stack_size(4096)
            .spawn(move || unsafe {
                if let [s0, m0, s1, m1] = nums[..] {
                    xtensa_perfmon_stop();
                    xtensa_perfmon_init(0, s0, m0, 0, 15);
                    xtensa_perfmon_init(1, s1, m1, 0, 15);
                    xtensa_perfmon_start();
                    (0, 0)
                } else {
                    xtensa_perfmon_stop();
                    (xtensa_perfmon_value(0), xtensa_perfmon_value(1))
                }
            })
            .map_err(|_| alloc::string::String::from("spawn"))?;
        let (a, b) = handle.join();
        Ok(Some(alloc::format!("{a} {b}")))
    }
}

pub fn register_commands() {
    #[cfg(target_arch = "xtensa")]
    harness_rs::register("perf", perf::command);
    #[cfg(target_arch = "xtensa")]
    harness_rs::register("prof", |args| {
        unsafe extern "C" {
            fn prof_start();
            fn prof_dump();
        }
        if args.first() == Some(&"dump") {
            drop(idf::thread::Builder::new().stack_size(4096).spawn(|| unsafe { prof_dump() }));
        } else {
            unsafe { prof_start() };
        }
        Ok(None)
    });
    harness_rs::register("cpu", cpu_command);
    harness_rs::register("udpflood", flood_command);
    harness_rs::register("diag", diag_command);
    #[cfg(CONFIG_SOC_WIFI_SUPPORTED)]
    harness_rs::register("wifistat", wifistat_command);
    harness_rs::register("heap", heap_command);
    harness_rs::register("wifi", wifi_command);
    harness_rs::register("net", net_command);
    harness_rs::register("tcp", |args| exchange_command(args, false));
    harness_rs::register("udp", |args| exchange_command(args, true));
    harness_rs::register("dns", dns_command);
}
