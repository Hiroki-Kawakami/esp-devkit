/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Host names to addresses, through the name servers of the interfaces.

mod cache;
pub(crate) mod message;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use core::time::Duration;

use idf::executor;
use idf::time::Instant;

use self::cache::{Cache, Record};
use self::message::{Name, RCODE_NXDOMAIN, Response};
use crate::net::{self, Net};
use crate::watch::Watch;
use crate::{Error, Result, TcpBuilder, UdpBuilder, UdpSocket};

const MAX_ALIASES: u8 = 8;
const ROUNDS: u32 = 4;
const FIRST_WAIT: Duration = Duration::from_secs(1);
const BUDGET: Duration = Duration::from_secs(10);
const TCP_WAIT: Duration = Duration::from_secs(5);
const UDP_MESSAGE: usize = 512;
const MAX_TCP_MESSAGE: usize = 16384;

/// Where a query ends: addresses, a name to ask about next, or nothing.
#[derive(Clone, PartialEq, Debug)]
struct Answer {
    aliases: u8,
    record: Record,
}

/// The addresses, and how many aliases led to them.
type Resolved = (Result<Vec<Ipv4Addr>>, u8);

type Slot = Arc<Watch<Option<Resolved>>>;

/// The cache and the lookups in flight, under the stack's lock.
#[derive(Default)]
pub(crate) struct State {
    cache: Cache,
    pending: Vec<(Name, Slot)>,
}

/// Addresses of `host`, a name or an address literal. `localhost` is
/// 127.0.0.1 without a query.
///
/// Blocks; on the stack's own task it never returns.
pub fn lookup(host: &str) -> Result<Vec<IpAddr>> {
    executor::block_on(lookup_async(host))
}

pub fn lookup_timeout(host: &str, timeout: Duration) -> Result<Vec<IpAddr>> {
    executor::block_on(executor::timeout(timeout, lookup_async(host))).map_err(|_| Error::TimedOut)?
}

/// The query goes on when the future is dropped, and its answer is cached.
pub async fn lookup_async(host: &str) -> Result<Vec<IpAddr>> {
    lookup_in(net::global()?, host).await
}

// The caller only reads the cache and waits; queries and the aliases after
// them are followed on the stack's task, which keeps this future and its
// poll small on the caller's stack.
pub(crate) async fn lookup_in(net: &'static Net, host: &str) -> Result<Vec<IpAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(alloc::vec![ip]);
    }
    let name = message::parse_name(host).ok_or(Error::InvalidInput)?;
    if name == b"localhost" {
        return Ok(alloc::vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    }
    let mut aliases = 0;
    let slot = {
        let mut inner = net.lock();
        match from_cache(&mut inner, name, &mut aliases) {
            Step::Done(result) => return v4s(result),
            Step::Ask(name) => join(net, &mut inner, name)?,
        }
    };
    let (result, more) = loop {
        if let (Some(resolved), _) = slot.after(0).await {
            break resolved;
        }
    };
    if aliases + more > MAX_ALIASES {
        return Err(Error::NotFound);
    }
    v4s(result)
}

fn v4s(result: Result<Vec<Ipv4Addr>>) -> Result<Vec<IpAddr>> {
    result.map(|addrs| addrs.into_iter().map(IpAddr::V4).collect())
}

enum Step {
    Done(Result<Vec<Ipv4Addr>>),
    /// Nothing cached for this name.
    Ask(Name),
}

fn from_cache(inner: &mut net::Inner, mut name: Name, aliases: &mut u8) -> Step {
    let servers = inner.stack.dns_generation();
    let now = net::now();
    loop {
        if *aliases > MAX_ALIASES {
            return Step::Done(Err(Error::NotFound));
        }
        match inner.dns.cache.get(&name, servers, now) {
            Some(Record::Alias(target)) => {
                *aliases += 1;
                name = target;
            }
            Some(Record::Addrs(addrs)) => return Step::Done(Ok(addrs)),
            Some(Record::Missing) => return Step::Done(Err(Error::NotFound)),
            None => return Step::Ask(name),
        }
    }
}

/// Joins the lookup of `name` in flight, or starts one on the stack's task.
fn join(net: &'static Net, inner: &mut net::Inner, name: Name) -> Result<Slot> {
    if let Some((_, slot)) = inner.dns.pending.iter().find(|(n, _)| *n == name) {
        return Ok(slot.clone());
    }
    let spawner = net.spawner().ok_or(Error::NotStarted)?;
    let slot: Slot = Arc::new(Watch::new(None));
    inner.dns.pending.push((name.clone(), slot.clone()));
    let done = slot.clone();
    spawner.spawn(async move {
        let resolved = resolve(net, name).await;
        net.lock_async().await.dns.pending.retain(|(_, s)| !Arc::ptr_eq(s, &done));
        done.set(Some(resolved));
    });
    Ok(slot)
}

/// Queries `name`, then whatever its aliases lead to that is not cached.
async fn resolve(net: &'static Net, name: Name) -> Resolved {
    let mut aliases = 0;
    let mut step = Step::Ask(name);
    loop {
        let name = match step {
            Step::Done(result) => return (result, aliases),
            Step::Ask(name) => name,
        };
        let answer = match query(net, &name).await {
            Ok(answer) => answer,
            Err(err) => return (Err(err), aliases),
        };
        aliases += answer.aliases;
        step = match answer.record {
            _ if aliases > MAX_ALIASES => Step::Done(Err(Error::NotFound)),
            Record::Addrs(addrs) => Step::Done(Ok(addrs)),
            Record::Missing => Step::Done(Err(Error::NotFound)),
            Record::Alias(target) => from_cache(&mut *net.lock_async().await, target, &mut aliases),
        };
    }
}

/// Asks the name servers in turn, each round waiting twice as long.
async fn query(net: &'static Net, name: &[u8]) -> Result<Answer> {
    let (servers, generation) = {
        let inner = net.lock_async().await;
        (inner.stack.name_servers(), inner.stack.dns_generation())
    };
    if servers.is_empty() {
        return Err(Error::NoNameServer);
    }
    let any = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
    let socket = UdpBuilder::default().rx_buffer(UDP_MESSAGE).bind_in(net, any)?;
    let id = idf::system::random() as u16;
    let request = message::query(id, name);
    let deadline = Instant::now() + BUDGET;
    let mut asked = Vec::new();
    let mut wait = FIRST_WAIT;
    for _ in 0..ROUNDS {
        let mut unsent = None;
        let mut sent = false;
        for &server in &servers {
            let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else {
                return Err(Error::TimedOut);
            };
            let to = SocketAddrV4::new(server, message::PORT);
            if let Err(err) = socket.send_to_async(&request, SocketAddr::V4(to)).await {
                unsent = Some(err);
                continue;
            }
            sent = true;
            if !asked.contains(&server) {
                asked.push(server);
            }
            let Some((from, mut response)) = receive(&socket, id, name, &asked, wait.min(left)).await else {
                continue;
            };
            if response.truncated {
                match over_tcp(net, from, &request, id, name).await {
                    Some(full) => response = full,
                    None => continue,
                }
            }
            if response.rcode == message::RCODE_NOERROR || response.rcode == RCODE_NXDOMAIN {
                return Ok(answer(net, name, &response, generation).await);
            }
        }
        if !sent && let Some(err) = unsent {
            return Err(err);
        }
        wait *= 2;
    }
    Err(Error::TimedOut)
}

/// The first response to the query from a server it went to.
async fn receive(
    socket: &UdpSocket,
    id: u16,
    name: &[u8],
    asked: &[Ipv4Addr],
    wait: Duration,
) -> Option<(Ipv4Addr, Response)> {
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; UDP_MESSAGE];
    loop {
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
        let (n, from) = executor::timeout(left, socket.recv_from_async(&mut buf)).await.ok()?.ok()?;
        let SocketAddr::V4(from) = from else { continue };
        if from.port() != message::PORT || !asked.contains(from.ip()) {
            continue;
        }
        if let Some(response) = message::decode(&buf[..n])
            && response.id == id
            && response.question == name
        {
            return Some((*from.ip(), response));
        }
    }
}

/// RFC 7766: the query again over TCP, for a truncated answer.
async fn over_tcp(net: &'static Net, server: Ipv4Addr, request: &[u8], id: u16, name: &[u8]) -> Option<Response> {
    let exchange = async {
        let to = SocketAddr::V4(SocketAddrV4::new(server, message::PORT));
        let stream = TcpBuilder::default().rx_buffer(4096).tx_buffer(1024).connect_to(net, to).await.ok()?;
        let mut framed = Vec::with_capacity(2 + request.len());
        framed.extend_from_slice(&(request.len() as u16).to_be_bytes());
        framed.extend_from_slice(request);
        stream.write_all_async(&framed).await.ok()?;
        let mut len = [0u8; 2];
        read_exact(&stream, &mut len).await?;
        let len = usize::from(u16::from_be_bytes(len));
        if len > MAX_TCP_MESSAGE {
            return None;
        }
        let mut buf = alloc::vec![0u8; len];
        read_exact(&stream, &mut buf).await?;
        message::decode(&buf).filter(|r| r.id == id && r.question == name && !r.truncated)
    };
    executor::timeout(TCP_WAIT, exchange).await.ok().flatten()
}

async fn read_exact(stream: &crate::TcpStream, mut buf: &mut [u8]) -> Option<()> {
    while !buf.is_empty() {
        let n = stream.read_async(buf).await.ok().filter(|&n| n > 0)?;
        buf = &mut buf[n..];
    }
    Some(())
}

/// Follows the CNAMEs from `name` within the response and caches every step.
async fn answer(net: &'static Net, name: &[u8], response: &Response, generation: u32) -> Answer {
    let mut steps: Vec<(Name, Record, u32)> = Vec::new();
    let mut current = name.to_vec();
    let mut aliases = 0;
    let record = loop {
        let addrs: Vec<(Ipv4Addr, u32)> =
            response.addrs.iter().filter(|(owner, ..)| *owner == current).map(|&(_, a, ttl)| (a, ttl)).collect();
        if !addrs.is_empty() {
            let ttl = addrs.iter().map(|&(_, ttl)| ttl).min().unwrap_or(0);
            let record = Record::Addrs(addrs.into_iter().map(|(a, _)| a).collect());
            steps.push((current, record.clone(), ttl));
            break record;
        }
        if aliases <= MAX_ALIASES
            && let Some((_, target, ttl)) = response.cnames.iter().find(|(owner, ..)| *owner == current)
        {
            steps.push((current, Record::Alias(target.clone()), *ttl));
            current = target.clone();
            aliases += 1;
            continue;
        }
        // An alias chain the server did not follow to the end is asked about
        // again from where it stops (RFC 1034 5.3.3); an SOA says it ends there.
        let ends = response.rcode == RCODE_NXDOMAIN || aliases == 0 || response.negative_ttl.is_some();
        if aliases > MAX_ALIASES || !ends {
            break Record::Alias(current);
        }
        if let Some(ttl) = response.negative_ttl {
            steps.push((current, Record::Missing, ttl));
        }
        break Record::Missing;
    };
    let mut inner = net.lock_async().await;
    if inner.stack.dns_generation() == generation {
        let now = net::now();
        for (name, record, ttl) in steps {
            inner.dns.cache.put(name, record, ttl, generation, now);
        }
    }
    Answer { aliases, record }
}
