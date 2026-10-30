/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

/// All-zero bytes are a valid `Self`. For a struct, implement it when every
/// field is `Zeroable`.
///
/// # Safety
/// The type must have no invalid all-zero value (references, `NonNull`,
/// enums without a zero discriminant).
pub unsafe trait Zeroable: Sized {}

macro_rules! zeroable {
    ($($t:ty),*) => { $(unsafe impl Zeroable for $t {})* };
}

zeroable!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f32, f64, bool);

unsafe impl<T: Zeroable, const N: usize> Zeroable for [T; N] {}
unsafe impl<T> Zeroable for *const T {}
unsafe impl<T> Zeroable for *mut T {}
