/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::{EastAsianWidth, GeneralCategory, compose_kana, east_asian_width, general_category, is_kana_voicing_mark};

#[test]
fn east_asian_widths() {
    assert_eq!(east_asian_width('A'), EastAsianWidth::Narrow);
    assert_eq!(east_asian_width('\u{00E9}'), EastAsianWidth::Ambiguous);
    assert_eq!(east_asian_width('\u{2500}'), EastAsianWidth::Ambiguous);
    assert_eq!(east_asian_width('\u{3042}'), EastAsianWidth::Wide);
    assert_eq!(east_asian_width('\u{6F22}'), EastAsianWidth::Wide);
    assert_eq!(east_asian_width('\u{FF21}'), EastAsianWidth::Fullwidth);
    assert_eq!(east_asian_width('\u{FF71}'), EastAsianWidth::Halfwidth);
    assert_eq!(east_asian_width('\u{1F600}'), EastAsianWidth::Wide);
    assert_eq!(east_asian_width('\u{0000}'), EastAsianWidth::Neutral);
    assert_eq!(east_asian_width('\u{10FFFF}'), EastAsianWidth::Neutral);
}

#[test]
fn unassigned_ideographs_default_to_wide() {
    assert_eq!(general_category('\u{2FFFD}'), GeneralCategory::Unassigned);
    assert_eq!(east_asian_width('\u{2FFFD}'), EastAsianWidth::Wide);
}

#[test]
fn general_categories() {
    assert_eq!(general_category('A'), GeneralCategory::UppercaseLetter);
    assert_eq!(general_category('a'), GeneralCategory::LowercaseLetter);
    assert_eq!(general_category('\u{0000}'), GeneralCategory::Control);
    assert_eq!(general_category('\u{0301}'), GeneralCategory::NonspacingMark);
    assert_eq!(general_category('\u{20DD}'), GeneralCategory::EnclosingMark);
    assert_eq!(general_category('\u{200B}'), GeneralCategory::Format);
    assert_eq!(general_category('\u{3099}'), GeneralCategory::NonspacingMark);
    assert_eq!(general_category('\u{E000}'), GeneralCategory::PrivateUse);
    assert_eq!(general_category('\u{10FFFF}'), GeneralCategory::Unassigned);
}

#[test]
fn kana_compose() {
    assert_eq!(compose_kana('\u{304B}', '\u{3099}'), Some('\u{304C}'));
    assert_eq!(compose_kana('\u{30CF}', '\u{309A}'), Some('\u{30D1}'));
    assert_eq!(compose_kana('\u{30A6}', '\u{3099}'), Some('\u{30F4}'));
    assert_eq!(compose_kana('\u{3042}', '\u{3099}'), None);
    assert_eq!(compose_kana('\u{304B}', '\u{309A}'), None);
    assert_eq!(compose_kana('\u{1F600}', '\u{3099}'), None);
    assert!(is_kana_voicing_mark('\u{309A}'));
    assert!(!is_kana_voicing_mark('\u{309B}'));
}
