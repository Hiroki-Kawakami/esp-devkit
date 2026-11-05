/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use idf::executor;

use crate::link::{Link, LinkInfo};
use crate::net::{self, Config, Ipv4Mode, wait_for};
use crate::stack::{IfaceId, IfaceStats, Ipv4Config, SocketId};
use crate::{Error, Result};

/// Starts the stack's task, once. Links may be added before or after.
pub fn start(config: Config) -> Result<()> {
    net::registry().start(config)
}

/// The interface starts with `Ipv4Mode::Off`.
pub fn add_link(name: &'static str, link: &'static dyn Link) -> Interface {
    Interface { id: net::registry().add_link(name, link) }
}

pub fn interfaces() -> Vec<Interface> {
    net::registry().lock().stack.iface_ids().map(|id| Interface { id }).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interface {
    id: IfaceId,
}

impl Interface {
    pub fn name(&self) -> &'static str {
        net::registry().lock().stack.name(self.id)
    }

    pub fn link_info(&self) -> LinkInfo {
        net::registry().lock().stack.link_info(self.id)
    }

    pub fn is_link_up(&self) -> bool {
        net::registry().lock().stack.is_link_up(self.id)
    }

    pub fn stats(&self) -> IfaceStats {
        net::registry().lock().stats(self.id)
    }

    /// `Static` and `Off` apply at once; `Dhcp` leases while the link is up
    /// and the stack runs.
    pub fn set_ipv4(&self, mode: Ipv4Mode) {
        let net = net::registry();
        let mut inner = net.lock();
        inner.modes[self.id.index()] = mode;
        match mode {
            Ipv4Mode::Static(config) => inner.stack.set_ipv4(self.id, Some(config)),
            Ipv4Mode::Off => inner.stack.set_ipv4(self.id, None),
            Ipv4Mode::Dhcp => {}
        }
        if mode != Ipv4Mode::Dhcp {
            inner.stack.set_dns_servers(self.id, &[]);
        }
        drop(inner);
        net.kick();
    }

    pub fn ipv4_mode(&self) -> Ipv4Mode {
        net::registry().lock().modes[self.id.index()]
    }

    pub fn ipv4(&self) -> Option<Ipv4Config> {
        net::registry().lock().stack.ipv4(self.id)
    }

    pub fn dns_servers(&self) -> Vec<Ipv4Addr> {
        net::registry().lock().stack.dns_servers(self.id).to_vec()
    }

    /// Only in `Static` mode; `set_ipv4` clears them.
    pub fn set_dns_servers(&self, servers: &[Ipv4Addr]) -> Result<()> {
        let mut inner = net::registry().lock();
        if !matches!(inner.modes[self.id.index()], Ipv4Mode::Static(_)) {
            return Err(Error::Unsupported);
        }
        inner.stack.set_dns_servers(self.id, servers);
        Ok(())
    }

    pub fn wait_ipv4(&self) -> Ipv4Config {
        executor::block_on(self.wait_ipv4_async())
    }

    pub fn wait_ipv4_timeout(&self, timeout: Duration) -> Result<Ipv4Config> {
        executor::block_on(executor::timeout(timeout, self.wait_ipv4_async())).map_err(|_| Error::TimedOut)
    }

    pub async fn wait_ipv4_async(&self) -> Ipv4Config {
        wait_for(net::registry(), SocketId::Iface(self.id), |inner, _| inner.stack.ipv4(self.id)).await
    }
}
