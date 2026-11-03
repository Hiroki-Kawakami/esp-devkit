/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Checks generated Rust code against the messages pbcheck_dump() wrote.
 */

const UNKNOWN_FIELDS: &[u8] = &[
    0x80, 0xb5, 0x18, 0xac, 0x02, // 100000: varint 300
    0x82, 0xb5, 0x18, 0x03, 0xaa, 0xaa, 0xaa, // 100000: 3 bytes
    0x85, 0xb5, 0x18, 0xbb, 0xbb, 0xbb, 0xbb, // 100000: fixed32
    0x81, 0xb5, 0x18, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, // 100000: fixed64
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn records(fixture: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = fixture;
    while !rest.is_empty() {
        let len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        out.push(&rest[4..4 + len]);
        rest = &rest[4 + len..];
    }
    out
}

fn check(i: usize, what: &str, actual: Option<Vec<u8>>, expect: &[u8]) {
    let Some(actual) = actual else { panic!("#{i}: {what}: not decoded") };
    if actual != expect {
        let at = actual.iter().zip(expect).position(|(a, b)| a != b).unwrap_or(actual.len().min(expect.len()));
        panic!("#{i}: {what}: {} bytes, expected {}, first difference at {at}", actual.len(), expect.len());
    }
}

/// `reencode` decodes a message and encodes it back, `None` when decoding
/// failed; `lazy_failed` reports a lazy field that failed in between.
pub fn run(
    fixture: &[u8],
    reencode: impl Fn(&[u8]) -> Option<Vec<u8>>,
    lazy_failed: impl Fn() -> bool,
) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let records = records(fixture);
    assert!(!records.is_empty());
    for (i, wire) in records.iter().enumerate() {
        lazy_failed();
        check(i, "round trip", reencode(wire), wire);
        assert!(!lazy_failed(), "#{i}: lazy field");

        let mut unknown = wire.to_vec();
        unknown.extend_from_slice(UNKNOWN_FIELDS);
        check(i, "unknown fields", reencode(&unknown), wire);
        assert!(!lazy_failed(), "#{i}: lazy field with unknown fields");

        if !wire.is_empty() {
            let mut fuzz = wire.to_vec();
            for _ in 0..rng.pick(4) {
                let at = rng.pick(fuzz.len());
                fuzz[at] ^= 1 << rng.pick(8);
            }
            let len = if rng.pick(2) == 0 { fuzz.len() } else { rng.pick(fuzz.len() + 1) };
            reencode(&fuzz[..len]);
            lazy_failed();
        }
    }
    println!("{} messages", records.len());
}
