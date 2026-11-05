/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod dhcp;
mod ipv4;
mod link;
mod runner;
mod tcp;
mod udp;
mod wifi_manager;

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use idf::thread;

use crate::net::{self, Net};
use crate::wire::Mac;
use crate::{Config, IfaceId, Ipv4Config, Link, LinkInfo, LinkRx, Medium, TxError};

pub(super) const MAC_A: Mac = [0x02, 0, 0, 0, 0, 0x0a];
pub(super) const MAC_B: Mac = [0x02, 0, 0, 0, 0, 0x0b];
pub(super) const IP_A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub(super) const IP_B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);

pub(super) fn eth(mac: Mac) -> LinkInfo {
    LinkInfo { medium: Medium::Ethernet, mac, mtu: 1500 }
}

pub(super) fn config(address: Ipv4Addr) -> Option<Ipv4Config> {
    Some(Ipv4Config { address, prefix_len: 24, gateway: None })
}

pub(super) struct TestLink {
    info: LinkInfo,
    out: Mutex<VecDeque<Vec<u8>>>,
}

impl Link for TestLink {
    fn info(&self) -> LinkInfo {
        self.info
    }

    fn attach(&self, _: LinkRx) {}

    fn transmit(&self, frame: &[u8]) -> Result<(), TxError> {
        self.out.lock().unwrap().push_back(frame.to_vec());
        Ok(())
    }
}

/// Two `Net`s on test links, with a task carrying frames between them.
pub(super) struct Live {
    pub a: &'static Net,
    pub b: &'static Net,
    pub a_iface: IfaceId,
    stop: Arc<AtomicBool>,
    pump: Option<thread::JoinHandle<()>>,
}

impl Live {
    pub fn new() -> Self {
        Self::with(config(IP_A), config(IP_B))
    }

    pub fn with(a: Option<Ipv4Config>, b: Option<Ipv4Config>) -> Self {
        let mut nets = Vec::new();
        let mut links = Vec::new();
        for (mac, ip) in [(MAC_A, a), (MAC_B, b)] {
            let net: &'static Net = Box::leak(Box::new(Net::with_config(Config::default())));
            let link: &'static TestLink = Box::leak(Box::new(TestLink { info: eth(mac), out: Mutex::new(VecDeque::new()) }));
            let id = net.add_link("eth", link);
            let mut inner = net.lock();
            inner.stack.set_ipv4(id, ip);
            inner.stack.set_link_up(id, true);
            drop(inner);
            nets.push((net, id));
            links.push(link);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let pump = {
            let stop = stop.clone();
            let (na, nb) = (nets[0], nets[1]);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for (from, (to, id)) in [(&links[0], nb), (&links[1], na)] {
                        let frames: Vec<_> = from.out.lock().unwrap().drain(..).collect();
                        for f in frames {
                            to.receive(id, &f, net::now());
                        }
                    }
                    na.0.poll(net::now());
                    nb.0.poll(net::now());
                    thread::sleep(Duration::from_millis(1));
                }
            })
        };
        Self { a: nets[0].0, b: nets[1].0, a_iface: nets[0].1, stop, pump: Some(pump) }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.pump.take().unwrap().join();
    }
}

// A wakeup reaches only FreeRTOS tasks, which the test's own thread is not.
pub(super) fn in_task<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    thread::spawn(f).join()
}

pub(super) fn v4(ip: Ipv4Addr, port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(ip, port))
}

