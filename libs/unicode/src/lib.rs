/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Unicode character properties. See README.md.
 */

#![cfg_attr(not(test), no_std)]

mod tables;
#[cfg(test)]
mod tests;

pub use tables::UNICODE_VERSION;

/// East_Asian_Width (UAX #11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EastAsianWidth {
    Ambiguous,
    Fullwidth,
    Halfwidth,
    Neutral,
    Narrow,
    Wide,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GeneralCategory {
    UppercaseLetter,
    LowercaseLetter,
    TitlecaseLetter,
    ModifierLetter,
    OtherLetter,
    NonspacingMark,
    SpacingMark,
    EnclosingMark,
    DecimalNumber,
    LetterNumber,
    OtherNumber,
    ConnectorPunctuation,
    DashPunctuation,
    OpenPunctuation,
    ClosePunctuation,
    InitialPunctuation,
    FinalPunctuation,
    OtherPunctuation,
    MathSymbol,
    CurrencySymbol,
    ModifierSymbol,
    OtherSymbol,
    SpaceSeparator,
    LineSeparator,
    ParagraphSeparator,
    Control,
    Format,
    Surrogate,
    PrivateUse,
    Unassigned,
}

const KANA_VOICED_MARK: char = '\u{3099}';
const KANA_SEMI_VOICED_MARK: char = '\u{309A}';

/// The value of the run in `table` that `cp` falls in.
fn lookup(table: &[u32], bits: u32, cp: u32) -> usize {
    let at = table.partition_point(|&entry| entry >> bits <= cp);
    (table[at - 1] & ((1 << bits) - 1)) as usize
}

pub fn east_asian_width(c: char) -> EastAsianWidth {
    tables::EAST_ASIAN_WIDTHS[lookup(&tables::EAST_ASIAN_WIDTH, 3, c as u32)]
}

pub fn general_category(c: char) -> GeneralCategory {
    tables::GENERAL_CATEGORIES[lookup(&tables::GENERAL_CATEGORY, 5, c as u32)]
}

/// The combining voiced and semi-voiced sound marks, U+3099 and U+309A.
pub fn is_kana_voicing_mark(c: char) -> bool {
    c == KANA_VOICED_MARK || c == KANA_SEMI_VOICED_MARK
}

/// The canonical composition of a kana and a voicing mark, `None` when the pair
/// has none.
pub fn compose_kana(base: char, mark: char) -> Option<char> {
    let (base, mark) = (u16::try_from(base as u32).ok()?, u16::try_from(mark as u32).ok()?);
    let at = tables::KANA.binary_search_by_key(&(base, mark), |&(b, m, _)| (b, m)).ok()?;
    char::from_u32(u32::from(tables::KANA[at].2))
}
