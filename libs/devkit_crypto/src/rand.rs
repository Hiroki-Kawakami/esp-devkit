/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::psa::{self, sys};

/// Cryptographically secure random bytes. Panics if the backend fails.
pub fn fill(out: &mut [u8]) {
    psa::init();
    psa::check(unsafe { sys::psa_generate_random(out.as_mut_ptr(), out.len()) });
}
