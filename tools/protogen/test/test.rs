/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

extern crate alloc;

mod pbcheck;

#[allow(dead_code, non_upper_case_globals)]
mod proto {
    include!(concat!(env!("PBCHECK_GEN"), "/test.pb.rs"));
}

use proto::pb::{Encode, Message};
use proto::{TestAll, TestAllPick, TestEmpty};

#[test]
fn matches_protobuf_c() {
    pbcheck::run(
        include_bytes!(env!("PBCHECK_FIXTURE")),
        |wire| TestAll::decode(wire).ok().map(|m| m.encode_to_vec()),
        proto::pb::take_lazy_failure,
    );
}

#[test]
fn unpacked_repeated() {
    // r_u32 = 1, 300 unpacked, then 5, 6 packed.
    let wire = [0x90, 0x01, 0x01, 0x90, 0x01, 0xAC, 0x02, 0x92, 0x01, 0x02, 0x05, 0x06];
    assert_eq!(TestAll::decode(&wire).unwrap().r_u32, [1, 300, 5, 6]);
}

#[test]
fn lazy_from_value() {
    let empty = TestEmpty::default();
    let child = TestAll { i32: -1, ..Default::default() };
    let m = TestAll { pick: Some(TestAllPick::OChild(proto::pb::Lazy::new(&child))), ..Default::default() };
    let wire = m.encode_to_vec();
    let TestAllPick::OChild(back) = TestAll::decode(&wire).unwrap().pick.unwrap() else { panic!() };
    assert_eq!(back.get().unwrap().i32, -1);
    assert!(proto::pb::Lazy::new(&empty).get().is_err());
}
