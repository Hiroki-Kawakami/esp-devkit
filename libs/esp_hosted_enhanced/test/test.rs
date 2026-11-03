/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

extern crate alloc;

#[path = "../../../tools/protogen/test/pbcheck.rs"]
mod pbcheck;

#[allow(dead_code, non_upper_case_globals)]
mod rpc {
    include!(concat!(env!("PBCHECK_GEN"), "/esp_hosted_rpc.pb.rs"));
}

use rpc::pb::{Encode, Message};

#[test]
fn matches_protobuf_c() {
    pbcheck::run(
        include_bytes!(env!("PBCHECK_FIXTURE")),
        |wire| rpc::Rpc::decode(wire).ok().map(|m| m.encode_to_vec()),
        rpc::pb::take_lazy_failure,
    );
}
