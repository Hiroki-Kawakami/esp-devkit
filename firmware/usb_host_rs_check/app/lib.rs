/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]
#![allow(unexpected_cfgs)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::CStr;
use core::future::{Future, poll_fn};
use core::pin::Pin;
use core::task::Poll;
use core::time::Duration;

use idf::executor;
use idf::heap::Caps;
use idf::log::{error, info};
use idf::sync::Mutex;
use idf::thread::Builder;
use idf::time::Instant;
use usb_host_rs::setup::request_type;
use usb_host_rs::{Buffer, Device, Endpoint, Event, Interface, Setup};

const TAG: &CStr = c"usb_host_rs_check";

const VENDOR_ID: u16 = 0x1209;
const PRODUCT_ID: u16 = 0x0001;
const VENDOR_CLASS: u8 = 0xff;
const REQ_PATTERN: u8 = 0x01;
const REQ_STORE: u8 = 0x02;
const REQ_LOAD: u8 = 0x03;
const REQ_UNKNOWN: u8 = 0x7f;

/// The longest message the loopback echoes in one go.
const MAX_MESSAGE: usize = 16384;
/// Longer than any echo, so each one ends at its short packet or ZLP.
const IN_BYTES: usize = MAX_MESSAGE + 512;
const ECHO_TIMEOUT: Duration = Duration::from_secs(2);
const CANCEL_AFTER: Duration = Duration::from_millis(100);
const CAPS: Caps = Caps::DMA.union(Caps::INTERNAL);

#[derive(Clone)]
struct Loopback {
    interface: Interface,
    out: Endpoint,
    input: Endpoint,
}

static LOOPBACK: Mutex<Option<Loopback>> = Mutex::new(None);

fn attach(device: Device) -> Option<Loopback> {
    let d = device.descriptor();
    if (d.vendor_id, d.product_id) != (VENDOR_ID, PRODUCT_ID) {
        return None;
    }
    let Some(number) =
        device.configuration().interfaces().find(|i| i.class == VENDOR_CLASS && i.alternate == 0).map(|i| i.number)
    else {
        error!(TAG, "no vendor interface");
        return None;
    };
    let interface = match device.claim(number, 0) {
        Ok(interface) => interface,
        Err(err) => {
            error!(TAG, "claim {number}: {err}");
            return None;
        }
    };
    let out = interface.endpoints().find(|e| !e.descriptor().is_in());
    let input = interface.endpoints().find(|e| e.descriptor().is_in());
    let (Some(out), Some(input)) = (out, input) else {
        error!(TAG, "interface {number} lacks a bulk pair");
        return None;
    };
    info!(TAG, "loopback on interface {number}: OUT {:02x} IN {:02x}", out.address(), input.address());
    Some(Loopback { interface, out, input })
}

async fn follow() {
    let mut watch = usb_host_rs::watch();
    loop {
        match watch.next().await {
            Event::Arrived(device) => {
                if let Some(loopback) = attach(device) {
                    *LOOPBACK.lock() = Some(loopback);
                }
            }
            Event::Left(device) => {
                let mut loopback = LOOPBACK.lock();
                if loopback.as_ref().is_some_and(|l| l.interface.device().id() == device.id()) {
                    *loopback = None;
                    info!(TAG, "loopback gone");
                }
            }
        }
    }
}

pub fn run() {
    let board = match bsp_rs::init(&bsp_rs::Config::default()) {
        Ok(board) => board,
        Err(err) => {
            error!(TAG, "bsp_rs::init: {err}");
            return;
        }
    };
    if let Err(err) = usb_host_rs::start(&usb_host_rs::Config::default()) {
        error!(TAG, "usb_host_rs::start: {err}");
    }
    match Builder::new().name("check").stack_size(8192).spawn_local_executor(|_| {}) {
        Ok(spawner) => drop(spawner.spawn(follow())),
        Err(err) => error!(TAG, "executor: {err}"),
    }
    harness_rs::register("dev", |_| dev_command());
    harness_rs::register("loop", loop_command);
    harness_rs::register("queue", queue_command);
    harness_rs::register("ctrl", |_| executor::block_on(ctrl()).map(Some));
    harness_rs::register("cancel", |_| executor::block_on(cancel()).map(Some));
    if let Err(err) = harness_rs::start(&board) {
        error!(TAG, "harness_rs::start: {err}");
    }
}

fn loopback() -> Result<Loopback, String> {
    LOOPBACK.lock().clone().ok_or_else(|| "no loopback device".to_string())
}

fn dev_command() -> Result<Option<String>, String> {
    let l = loopback()?;
    let device = l.interface.device();
    let d = device.descriptor();
    Ok(Some(format!(
        "{:04x}:{:04x} {:?} \"{}\" \"{}\" \"{}\"",
        d.vendor_id,
        d.product_id,
        device.speed(),
        device.manufacturer(),
        device.product(),
        device.serial()
    )))
}

fn arg(args: &[&str], at: usize, default: usize) -> Result<usize, String> {
    args.get(at).map_or(Ok(default), |a| a.parse().map_err(|_| format!("bad number {a}")))
}

fn pattern(i: usize, seed: usize) -> u8 {
    (i.wrapping_mul(31) ^ seed.wrapping_mul(97)) as u8
}

fn fill(buf: &mut [u8], seed: usize) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = pattern(i, seed);
    }
}

fn buffer(len: usize) -> Result<Buffer, String> {
    Buffer::new(len, CAPS).map_err(|e| format!("buffer: {e}"))
}

async fn send(l: &Loopback, buf: Buffer, len: usize) -> Result<Buffer, String> {
    let (buf, r) = match executor::timeout(ECHO_TIMEOUT, l.out.transfer_zlp(buf, len)).await {
        Ok(done) => done,
        Err(_) => return Err(format!("OUT {len}: not taken")),
    };
    match r {
        Ok(n) if n == len => Ok(buf),
        Ok(n) => Err(format!("OUT moved {n} of {len}")),
        Err(e) => Err(format!("OUT: {e}")),
    }
}

async fn receive(l: &Loopback, buf: Buffer, seed: usize, len: usize) -> Result<Buffer, String> {
    let (buf, r) = match executor::timeout(ECHO_TIMEOUT, l.input.transfer(buf, IN_BYTES)).await {
        Ok(done) => done,
        Err(_) => return Err(format!("echo {seed}: no answer")),
    };
    let n = r.map_err(|e| format!("IN: {e}"))?;
    if n != len {
        return Err(format!("echo {seed}: {n} bytes, sent {len}"));
    }
    for (i, &b) in buf[..n].iter().enumerate() {
        let want = pattern(i, seed);
        if b != want {
            return Err(format!("echo {seed}: byte {i} is {b:02x}, sent {want:02x}"));
        }
    }
    Ok(buf)
}

fn loop_command(args: &[&str]) -> Result<Option<String>, String> {
    let len = arg(args, 0, 512)?;
    let count = arg(args, 1, 1)?;
    if len > MAX_MESSAGE {
        return Err(format!("at most {MAX_MESSAGE} bytes"));
    }
    let l = loopback()?;
    executor::block_on(async {
        let mut out = buffer(MAX_MESSAGE)?;
        let mut input = buffer(IN_BYTES)?;
        let start = Instant::now();
        for i in 0..count {
            fill(&mut out[..len], i);
            out = send(&l, out, len).await?;
            input = receive(&l, input, i, len).await?;
        }
        let us = start.elapsed().as_micros().max(1) as u64;
        let kbps = (len * count) as u64 * 2 * 1000 / us;
        Ok(Some(format!("{count} x {len} in {} us, {kbps} kB/s both ways", us)))
    })
}

type Job = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// Polls every job in order until all are done; the first poll of each runs
/// in that order, so their transfers queue in it.
async fn join_all(mut jobs: Vec<Job>) -> Result<(), String> {
    let mut results: Vec<Option<Result<(), String>>> = (0..jobs.len()).map(|_| None).collect();
    poll_fn(|cx| {
        for (job, result) in jobs.iter_mut().zip(results.iter_mut()) {
            if result.is_none()
                && let Poll::Ready(r) = job.as_mut().poll(cx)
            {
                *result = Some(r);
            }
        }
        if results.iter().all(Option::is_some) { Poll::Ready(()) } else { Poll::Pending }
    })
    .await;
    results.into_iter().flatten().collect()
}

fn queue_len(k: usize) -> usize {
    (100 + k * 4093) % MAX_MESSAGE + 1
}

fn queue_command(args: &[&str]) -> Result<Option<String>, String> {
    let depth = arg(args, 0, 4)?;
    let l = loopback()?;
    let mut jobs: Vec<Job> = Vec::new();
    for k in 0..depth {
        let l = l.clone();
        let input = buffer(IN_BYTES)?;
        jobs.push(Box::pin(async move { receive(&l, input, k, queue_len(k)).await.map(drop) }));
    }
    {
        let l = l.clone();
        let mut out = buffer(MAX_MESSAGE)?;
        jobs.push(Box::pin(async move {
            for k in 0..depth {
                fill(&mut out[..queue_len(k)], k);
                out = send(&l, out, queue_len(k)).await?;
            }
            Ok(())
        }));
    }
    executor::block_on(join_all(jobs))?;
    Ok(Some(format!("{depth} IN transfers in flight, echoes in order")))
}

fn vendor(direction: u8, request: u8, length: u16) -> Setup {
    Setup { request_type: direction | request_type::VENDOR | request_type::DEVICE, request, value: 0, index: 0, length }
}

async fn ctrl() -> Result<String, String> {
    let device = loopback()?.interface.device().clone();
    let (buf, r) = device.control_in(vendor(request_type::IN, REQ_PATTERN, 200), buffer(200)?).await;
    let n = r.map_err(|e| format!("pattern: {e}"))?;
    if n != 200 || buf[..n].iter().enumerate().any(|(i, &b)| b != i as u8) {
        return Err(format!("pattern: {n} bytes, wrong content"));
    }

    let mut stored = buffer(64)?;
    fill(&mut stored, 5);
    let (stored, r) = device.control_out(vendor(0, REQ_STORE, 64), stored).await;
    r.map_err(|e| format!("store: {e}"))?;
    let (loaded, r) = device.control_in(vendor(request_type::IN, REQ_LOAD, 64), buffer(64)?).await;
    let n = r.map_err(|e| format!("load: {e}"))?;
    if loaded[..n] != stored[..] {
        return Err("load: differs from what was stored".into());
    }

    let (_, r) = device.control_in(vendor(request_type::IN, REQ_UNKNOWN, 8), buffer(8)?).await;
    if r != Err(usb_host_rs::Error::Stall) {
        return Err(format!("unknown request: {r:?}, expected a stall"));
    }
    Ok("pattern, store/load and stall".into())
}

async fn cancel() -> Result<String, String> {
    let l = loopback()?;
    if executor::timeout(CANCEL_AFTER, l.input.transfer(buffer(IN_BYTES)?, IN_BYTES)).await.is_ok() {
        return Err("IN completed with nothing sent".into());
    }
    let mut out = buffer(MAX_MESSAGE)?;
    fill(&mut out[..300], 9);
    send(&l, out, 300).await?;
    receive(&l, buffer(IN_BYTES)?, 9, 300).await?;
    Ok("canceled IN, next echo intact".into())
}
