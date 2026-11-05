/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod blake2s;
mod sha;

use core::fmt;

pub use blake2s::Blake2s;
pub use sha::{Sha1, Sha256, Sha384, Sha512};

use crate::psa;

/// The hash a signature is computed over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HashAlg {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl HashAlg {
    pub fn output_len(self) -> usize {
        match self {
            HashAlg::Sha1 => 20,
            HashAlg::Sha256 => 32,
            HashAlg::Sha384 => 48,
            HashAlg::Sha512 => 64,
        }
    }

    pub(crate) fn psa(self) -> u32 {
        match self {
            HashAlg::Sha1 => psa::ALG_SHA_1,
            HashAlg::Sha256 => psa::ALG_SHA_256,
            HashAlg::Sha384 => psa::ALG_SHA_384,
            HashAlg::Sha512 => psa::ALG_SHA_512,
        }
    }
}

/// Panics if the backend fails, which only a hardware fault causes.
pub trait Hash: Clone {
    const BLOCK: usize;
    type Output: Copy + Eq + fmt::Debug + AsRef<[u8]> + AsMut<[u8]>;
    const ZERO: Self::Output;

    fn new() -> Self;
    fn update(&mut self, data: &[u8]);
    fn finalize(self) -> Self::Output;

    fn digest(data: &[u8]) -> Self::Output {
        let mut hash = Self::new();
        hash.update(data);
        hash.finalize()
    }
}
