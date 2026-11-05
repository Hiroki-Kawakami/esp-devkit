/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::wifi_manager::store::{Cipher, Saved};
use crate::wifi_manager::{entries, rank};
use crate::{ScanRecord, Security};

const MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x01];

fn record(ssid: &str, rssi: i8, channel: u8) -> ScanRecord {
    ScanRecord { ssid: ssid.as_bytes().to_vec(), bssid: [0x02, 0, 0, 0, 0, channel], channel, rssi, security: Security::Wpa2Psk }
}

fn saved(ssid: &str, hidden: bool, seq: u32) -> Saved {
    Saved { ssid: ssid.as_bytes().to_vec(), password: String::from("placeholder"), hidden, seq }
}

#[test]
fn saved_networks_round_trip_sealed() {
    let cipher = Cipher::new(MAC);
    let network = saved("example-ap", true, 7);
    let blob = cipher.seal(&network.encode(), [9; 12]);
    assert!(!blob.windows(11).any(|w| w == b"placeholder"));
    assert!(!blob.windows(10).any(|w| w == b"example-ap"));
    assert_eq!(cipher.open(&blob).as_deref().and_then(Saved::decode), Some(network));

    let open = Saved { password: String::new(), ..saved("example-open", false, 0) };
    assert_eq!(Saved::decode(&open.encode()), Some(open));
}

#[test]
fn another_chip_or_damage_reads_nothing() {
    let blob = Cipher::new(MAC).seal(&saved("example-ap", false, 1).encode(), [1; 12]);
    assert_eq!(Cipher::new([0x02, 0, 0, 0, 0, 0x02]).open(&blob), None);
    let mut damaged = blob.clone();
    damaged[20] ^= 1;
    assert_eq!(Cipher::new(MAC).open(&damaged), None);
    assert_eq!(Cipher::new(MAC).open(&blob[..10]), None);
    assert_eq!(Saved::decode(&[3, b'a']), None);
}

#[test]
fn ranking_prefers_signal_with_a_bonus_for_the_last_joined() {
    let saved = [saved("a", false, 5), saved("b", false, 1), saved("weak", false, 9), saved("gone", false, 2)];
    let records = [record("a", -60, 1), record("b", -55, 6), record("b", -75, 11), record("weak", -85, 3), record("other", -30, 9)];
    let order: Vec<_> = rank(&records, &saved, |_| false).into_iter().map(|(s, ch)| (s.ssid.clone(), ch)).collect();
    assert_eq!(order, vec![(b"b".to_vec(), Some(6)), (b"a".to_vec(), Some(1)), (b"weak".to_vec(), Some(3))]);

    let saved = [saved[0].clone(), saved[1].clone()];
    let order: Vec<_> = rank(&records, &saved, |_| false).into_iter().map(|(s, _)| s.ssid.clone()).collect();
    assert_eq!(order, vec![b"a".to_vec(), b"b".to_vec()]);
}

#[test]
fn ranking_skips_and_appends_unseen_hidden_networks() {
    let saved = [saved("a", false, 1), saved("hidden-old", true, 2), saved("hidden-new", true, 3), saved("skip", false, 4)];
    let records = [record("a", -50, 1), record("skip", -40, 2)];
    let order: Vec<_> =
        rank(&records, &saved, |ssid| ssid == b"skip").into_iter().map(|(s, ch)| (s.ssid.clone(), ch)).collect();
    assert_eq!(order, vec![(b"a".to_vec(), Some(1)), (b"hidden-new".to_vec(), None), (b"hidden-old".to_vec(), None)]);
}

#[test]
fn scan_entries_merge_by_ssid_and_sort() {
    let saved = [saved("mine", false, 1), saved("home", false, 2)];
    let records = [record("loud", -30, 1), record("mine", -70, 2), record("mine", -60, 3), record("", -20, 4), record("home", -80, 5)];
    let list = entries(&records, &saved, Some(b"home"));
    let names: Vec<_> = list.iter().map(|e| (e.ssid.as_slice(), e.rssi, e.saved, e.connected)).collect();
    assert_eq!(names, vec![(&b"home"[..], -80, true, true), (b"mine", -60, true, false), (b"loud", -30, false, false)]);
    assert_eq!(list[1].channel, 3);
}
