/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use crate::net::Net;
use crate::{Error, Result, dns};

mod private {
    pub enum Target<'a> {
        Addrs(alloc::vec::Vec<core::net::SocketAddr>),
        Host(&'a str, u16),
    }
}

use private::Target;

/// What `connect` takes, as with `std::net::ToSocketAddrs`: socket
/// addresses, or a host and a port, the host looked up with `dns::lookup`.
pub trait ToSocketAddrs {
    #[doc(hidden)]
    fn target(&self) -> Result<Target<'_>>;
}

pub(crate) async fn resolve(net: &'static Net, target: Target<'_>) -> Result<Vec<SocketAddr>> {
    match target {
        Target::Addrs(addrs) => Ok(addrs),
        Target::Host(host, port) => {
            Ok(dns::lookup_in(net, host).await?.into_iter().map(|ip| SocketAddr::new(ip, port)).collect())
        }
    }
}

impl ToSocketAddrs for SocketAddr {
    fn target(&self) -> Result<Target<'_>> {
        Ok(Target::Addrs(alloc::vec![*self]))
    }
}

impl ToSocketAddrs for SocketAddrV4 {
    fn target(&self) -> Result<Target<'_>> {
        Ok(Target::Addrs(alloc::vec![SocketAddr::V4(*self)]))
    }
}

impl ToSocketAddrs for SocketAddrV6 {
    fn target(&self) -> Result<Target<'_>> {
        Ok(Target::Addrs(alloc::vec![SocketAddr::V6(*self)]))
    }
}

impl ToSocketAddrs for (IpAddr, u16) {
    fn target(&self) -> Result<Target<'_>> {
        Ok(Target::Addrs(alloc::vec![SocketAddr::new(self.0, self.1)]))
    }
}

impl ToSocketAddrs for (Ipv4Addr, u16) {
    fn target(&self) -> Result<Target<'_>> {
        Ok(Target::Addrs(alloc::vec![SocketAddr::new(IpAddr::V4(self.0), self.1)]))
    }
}

impl ToSocketAddrs for (Ipv6Addr, u16) {
    fn target(&self) -> Result<Target<'_>> {
        Ok(Target::Addrs(alloc::vec![SocketAddr::new(IpAddr::V6(self.0), self.1)]))
    }
}

impl ToSocketAddrs for (&str, u16) {
    fn target(&self) -> Result<Target<'_>> {
        Ok(Target::Host(self.0, self.1))
    }
}

impl ToSocketAddrs for (String, u16) {
    fn target(&self) -> Result<Target<'_>> {
        Ok(Target::Host(&self.0, self.1))
    }
}

/// `host:port`, or an address with a port as `SocketAddr` parses it.
impl ToSocketAddrs for str {
    fn target(&self) -> Result<Target<'_>> {
        if let Ok(addr) = self.parse::<SocketAddr>() {
            return Ok(Target::Addrs(alloc::vec![addr]));
        }
        let (host, port) = self.rsplit_once(':').ok_or(Error::InvalidInput)?;
        Ok(Target::Host(host, port.parse().map_err(|_| Error::InvalidInput)?))
    }
}

impl ToSocketAddrs for String {
    fn target(&self) -> Result<Target<'_>> {
        self.as_str().target()
    }
}

impl ToSocketAddrs for [SocketAddr] {
    fn target(&self) -> Result<Target<'_>> {
        Ok(Target::Addrs(self.to_vec()))
    }
}

impl<T: ToSocketAddrs + ?Sized> ToSocketAddrs for &T {
    fn target(&self) -> Result<Target<'_>> {
        (**self).target()
    }
}
