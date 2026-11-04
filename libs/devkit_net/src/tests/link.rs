/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::harness::Net;
use crate::link::RxSink;
use crate::{IfaceId, IfaceStats, Instant, LinkInfo, LinkRx, Medium, RxFrame, Stack};

const ETH: LinkInfo = LinkInfo { medium: Medium::Ethernet, mac: [0x02, 0, 0, 0, 0, 1], mtu: 1500 };

fn stats(rx_frames: u32, rx_dropped: u32, tx_frames: u32, tx_dropped: u32) -> IfaceStats {
    IfaceStats { rx_frames, rx_dropped, rx_overflow: 0, tx_frames, tx_dropped }
}

#[test]
fn loopback_is_first_and_up() {
    let mut stack = Stack::new();
    assert_eq!(stack.name(IfaceId::LOOPBACK), "lo");
    assert!(stack.is_link_up(IfaceId::LOOPBACK));
    assert_eq!(stack.link_info(IfaceId::LOOPBACK).medium, Medium::Ip);
    let eth = stack.add_interface("sta", ETH);
    assert_ne!(eth, IfaceId::LOOPBACK);
    assert!(!stack.is_link_up(eth));
}

#[test]
fn receive_checks_state_and_length() {
    let mut stack = Stack::new();
    let eth = stack.add_interface("sta", ETH);
    stack.receive(eth, &[0; 60], Instant::ZERO, &mut |_, _| true);
    stack.set_link_up(eth, true);
    stack.receive(eth, &[0; 14], Instant::ZERO, &mut |_, _| true);
    stack.receive(eth, &[0; 1515], Instant::ZERO, &mut |_, _| true);
    stack.receive(eth, &[0; 1514], Instant::ZERO, &mut |_, _| true);
    stack.receive(eth, &[0; 60], Instant::ZERO, &mut |_, _| true);
    assert_eq!(stack.stats(eth), stats(2, 3, 0, 0));
}

#[test]
fn emit_skips_down_links() {
    let mut stack = Stack::new();
    let eth = stack.add_interface("sta", ETH);
    let mut sent = 0;
    stack.emit(eth, &[0; 60], &mut |_, _| {
        sent += 1;
        true
    });
    stack.set_link_up(eth, true);
    stack.emit(eth, &[0; 60], &mut |_, _| {
        sent += 1;
        true
    });
    stack.emit(eth, &[0; 60], &mut |_, _| false);
    assert_eq!(sent, 1);
    assert_eq!(stack.stats(eth), stats(0, 0, 1, 2));
}

#[test]
fn loopback_delivers_on_next_poll() {
    let mut stack = Stack::new();
    let mut tx = |_: IfaceId, _: &[u8]| panic!("loopback reached a link");
    stack.emit(IfaceId::LOOPBACK, &[0x45; 20], &mut tx);
    assert_eq!(stack.stats(IfaceId::LOOPBACK).rx_frames, 0);
    assert_eq!(stack.poll(Instant::ZERO, &mut tx), None);
    assert_eq!(stack.stats(IfaceId::LOOPBACK), stats(1, 0, 1, 0));
}

#[test]
fn loopback_queue_is_bounded() {
    let mut stack = Stack::new();
    let mut tx = |_: IfaceId, _: &[u8]| true;
    for _ in 0..20 {
        stack.emit(IfaceId::LOOPBACK, &[0x45; 20], &mut tx);
    }
    stack.poll(Instant::ZERO, &mut tx);
    assert_eq!(stack.stats(IfaceId::LOOPBACK), stats(16, 0, 16, 4));
}

#[test]
fn wire_delivers_after_its_delay() {
    let mut net = Net::new();
    let a = net.add_stack(Stack::new());
    let b = net.add_stack(Stack::new());
    let a_eth = net.stacks[a].add_interface("eth", ETH);
    let b_eth = net.stacks[b].add_interface("eth", ETH);
    net.connect((a, a_eth), (b, b_eth), Duration::from_millis(5));

    net.send((a, a_eth), &[0; 60]);
    net.run_for(Duration::from_millis(4));
    assert_eq!(net.stacks[b].stats(b_eth).rx_frames, 0);
    net.run_for(Duration::from_millis(1));
    assert_eq!(net.stacks[b].stats(b_eth).rx_frames, 1);
    assert_eq!(net.now, Instant::from_micros(5000));
    assert_eq!(net.captured.len(), 1);
    assert_eq!(net.captured[0].from, (a, a_eth));
    assert_eq!(net.captured[0].at, Instant::ZERO);
    assert_eq!(net.captured[0].frame, vec![0; 60]);
}

#[test]
fn instant_arithmetic() {
    let t = Instant::from_micros(1500) + Duration::from_millis(2);
    assert_eq!(t.as_micros(), 3500);
    assert_eq!(t - Instant::from_micros(500), Duration::from_millis(3));
    assert_eq!(Instant::from_micros(500) - t, Duration::ZERO);
}

static RELEASED: AtomicUsize = AtomicUsize::new(0);

#[test]
fn rx_frame_releases_once() {
    unsafe fn release(_: NonNull<u8>, len: usize, ctx: usize) {
        assert_eq!((len, ctx), (4, 7));
        RELEASED.fetch_add(1, Ordering::Relaxed);
    }
    let mut data = [1u8, 2, 3, 4];
    let frame = unsafe { RxFrame::from_raw(NonNull::new(data.as_mut_ptr()).unwrap(), 4, 7, release) };
    assert_eq!(&*frame, &[1, 2, 3, 4]);
    let moved = std::thread::spawn(move || drop(frame));
    moved.join().unwrap();
    assert_eq!(RELEASED.load(Ordering::Relaxed), 1);

    let boxed = RxFrame::from(vec![9u8; 3].into_boxed_slice());
    assert_eq!(&*boxed, &[9, 9, 9]);
}

#[derive(Default)]
struct Sink {
    frames: Mutex<Vec<Vec<u8>>>,
    up: Mutex<Option<bool>>,
}

impl RxSink for Sink {
    fn deliver(&self, frame: RxFrame) -> bool {
        self.frames.lock().unwrap().push(frame.to_vec());
        true
    }

    fn set_up(&self, up: bool) {
        *self.up.lock().unwrap() = Some(up);
    }

    fn tx_ready(&self) {}
}

#[test]
fn link_rx_forwards_to_the_stack_side() {
    let sink = Arc::new(Sink::default());
    let rx = LinkRx::new(sink.clone());
    let rx2 = rx.clone();
    assert!(rx2.deliver(RxFrame::from(vec![1u8, 2].into_boxed_slice())));
    rx.set_up(true);
    assert_eq!(*sink.frames.lock().unwrap(), vec![vec![1, 2]]);
    assert_eq!(*sink.up.lock().unwrap(), Some(true));
}
