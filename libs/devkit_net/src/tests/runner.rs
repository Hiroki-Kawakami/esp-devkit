/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use idf::{executor, thread};

use super::dhcp::{LEASED, Renewals, serve};
use super::{IP_B, MAC_A, MAC_B, TestLink, config, eth, in_task};
use crate::net::{self, Net};
use crate::{
    BoxFuture, Config, ConnectError, Error, Ipv4Mode, Link, LinkInfo, LinkRx, RxFrame, StaConfig, TcpBuilder,
    TcpStream, TxError, WifiDriver, WifiEvents, WifiState,
};

const PASSWORD: &str = "placeholder";

struct FakeLink {
    out: Mutex<VecDeque<Vec<u8>>>,
    rx: OnceLock<LinkRx>,
}

impl Link for FakeLink {
    fn info(&self) -> LinkInfo {
        eth(MAC_A)
    }

    fn attach(&self, rx: LinkRx) {
        self.rx.set(rx).ok().unwrap();
    }

    fn transmit(&self, frame: &[u8]) -> Result<(), TxError> {
        self.out.lock().unwrap().push_back(frame.to_vec());
        Ok(())
    }
}

struct FakeWifi {
    link: &'static FakeLink,
    events: OnceLock<WifiEvents>,
}

impl WifiDriver for FakeWifi {
    fn sta(&self) -> &'static dyn Link {
        self.link
    }

    fn attach(&self, events: WifiEvents) {
        self.events.set(events).ok().unwrap();
    }

    fn start(&self) -> BoxFuture<'_, Result<(), i32>> {
        Box::pin(async { Ok(()) })
    }

    fn stop(&self) -> BoxFuture<'_, Result<(), i32>> {
        Box::pin(async move {
            self.link.rx.get().unwrap().set_up(false);
            Ok(())
        })
    }

    fn scan(&self) -> BoxFuture<'_, Result<Vec<crate::ScanRecord>, i32>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn rssi(&self) -> BoxFuture<'_, Result<i8, i32>> {
        Box::pin(async { Ok(-40) })
    }

    fn connect<'a>(&'a self, config: &'a StaConfig<'a>) -> BoxFuture<'a, Result<(), i32>> {
        Box::pin(async move {
            let events = self.events.get().unwrap();
            if config.auth == crate::Auth::Psk(PASSWORD) {
                events.set_state(WifiState::Connected { bssid: MAC_B, channel: 6 });
                self.link.rx.get().unwrap().set_up(true);
            } else {
                events.set_state(WifiState::Disconnected(Some(crate::DisconnectReason::from_code(15))));
            }
            Ok(())
        })
    }

    fn set_power_save(&self, _: crate::PowerSave) -> BoxFuture<'_, Result<(), i32>> {
        Box::pin(async { Ok(()) })
    }

    fn disconnect(&self) -> BoxFuture<'_, Result<(), i32>> {
        Box::pin(async move {
            self.link.rx.get().unwrap().set_up(false);
            self.events.get().unwrap().set_state(WifiState::Disconnected(Some(crate::DisconnectReason::from_code(8))));
            Ok(())
        })
    }
}

/// The global stack on a fake radio, wired to a peer `Net` with a DHCP
/// server and a static address.
fn peer_for(link: &'static FakeLink) -> (&'static Net, Arc<AtomicBool>) {
    let peer: &'static Net = Box::leak(Box::new(Net::with_config(Config::default())));
    let peer_link: &'static TestLink =
        Box::leak(Box::new(TestLink { info: eth(MAC_B), out: Mutex::new(VecDeque::new()) }));
    let id = peer.add_link("eth", peer_link);
    {
        let mut inner = peer.lock();
        inner.stack.set_ipv4(id, config(IP_B));
        inner.stack.set_link_up(id, true);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let running = stop.clone();
    thread::spawn(move || {
        while !running.load(Ordering::Relaxed) {
            let frames: Vec<_> = link.out.lock().unwrap().drain(..).collect();
            for f in frames {
                peer.receive(id, &f, net::now());
            }
            let frames: Vec<_> = peer_link.out.lock().unwrap().drain(..).collect();
            if let Some(rx) = link.rx.get() {
                for f in frames {
                    rx.deliver(RxFrame::from(f.into_boxed_slice()));
                }
            }
            peer.poll(net::now());
            thread::sleep(Duration::from_millis(1));
        }
    });
    (peer, stop)
}

#[test]
fn wifi_dhcp_and_tcp_through_the_stack_task() {
    in_task(|| {
        let link: &'static FakeLink = Box::leak(Box::new(FakeLink { out: Mutex::new(VecDeque::new()), rx: OnceLock::new() }));
        let driver: &'static FakeWifi = Box::leak(Box::new(FakeWifi { link, events: OnceLock::new() }));
        let (peer, stop) = peer_for(link);

        let sta = crate::register_wifi(driver).unwrap();
        assert!(crate::register_wifi(driver).is_none());
        assert_eq!(sta.ipv4_mode(), Ipv4Mode::Dhcp);
        assert_eq!(TcpStream::connect(SocketAddr::V4(SocketAddrV4::new(IP_B, 7))).err(), Some(Error::NotStarted));
        crate::start(Config::default()).unwrap();
        assert_eq!(crate::start(Config::default()), Err(Error::AlreadyStarted));

        let wifi = crate::wifi().unwrap();
        let wrong = StaConfig::new("example-ap", "wrong");
        assert_eq!(executor::block_on(wifi.connect(&wrong)), Err(ConnectError::Stopped));
        executor::block_on(wifi.start()).unwrap();
        assert_eq!(executor::block_on(wifi.connect(&wrong)), Err(ConnectError::AuthFailed));
        assert!(!sta.is_link_up());

        let _server = serve(peer, 3600, Renewals::Answer, Duration::from_secs(10));
        executor::block_on(wifi.connect(&StaConfig::new("example-ap", PASSWORD))).unwrap();
        assert!(matches!(wifi.state(), WifiState::Connected { .. }));
        assert!(sta.is_link_up());
        let lease = sta.wait_ipv4_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(lease.address, LEASED);
        assert_eq!(crate::interfaces(), vec![sta]);

        let listener = TcpBuilder::default().listen_in(peer, SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 7))).unwrap();
        let echo = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut buf = [0; 64];
            let n = stream.read(&mut buf).unwrap();
            stream.write_all(&buf[..n]).unwrap();
        });
        let stream = TcpStream::connect(SocketAddr::V4(SocketAddrV4::new(IP_B, 7))).unwrap();
        stream.write_all(b"through the runner").unwrap();
        let mut buf = [0; 64];
        let n = stream.read_timeout(&mut buf, Duration::from_secs(2)).unwrap();
        assert_eq!(&buf[..n], b"through the runner");
        echo.join();

        executor::block_on(wifi.disconnect()).unwrap();
        assert!(!sta.is_link_up());
        let deadline = idf::time::Instant::now() + Duration::from_secs(1);
        while sta.ipv4().is_some() && idf::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(sta.ipv4(), None);
        stop.store(true, Ordering::Relaxed);
    });
}
