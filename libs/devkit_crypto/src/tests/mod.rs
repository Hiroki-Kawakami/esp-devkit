/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod asymmetric;
mod ca_bundle;
mod ed25519;
mod primitives;
mod tls;
mod x509;

pub(crate) fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1103515245).wrapping_add(12345) & 0x7FFF_FFFF;
            (x >> 16) as u8
        })
        .collect()
}

pub(crate) fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

pub(crate) fn vectors(kind: &str) -> Vec<Vec<String>> {
    let dir = std::env::var("DEVKIT_TEST_FIXTURES").expect("run test/run.sh, which generates the fixtures");
    let text = std::fs::read_to_string(format!("{dir}/vectors.txt")).unwrap();
    let rows: Vec<Vec<String>> = text
        .lines()
        .map(|line| line.split(' ').map(String::from).collect::<Vec<_>>())
        .filter(|row| row[0] == kind)
        .map(|row| row[1..].to_vec())
        .collect();
    assert!(!rows.is_empty());
    rows
}

pub(crate) fn num(s: &str) -> usize {
    s.parse().unwrap()
}


pub(crate) fn fixture(name: &str) -> Vec<u8> {
    let dir = std::env::var("DEVKIT_TEST_FIXTURES").expect("run test/run.sh, which generates the fixtures");
    std::fs::read(format!("{dir}/{name}")).unwrap()
}
