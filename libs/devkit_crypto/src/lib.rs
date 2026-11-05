/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
// CONFIG_DEVKIT_CRYPTO_CA_* are defined only for the choice Kconfig made.
#![allow(unexpected_cfgs)]

extern crate alloc;

pub mod aead;
pub mod ca_bundle;
pub mod cipher;
pub mod ct;
pub mod der;
pub mod ecdh;
pub mod ecdsa;
pub mod ed25519;
mod error;
pub mod hash;
pub mod kdf;
pub mod mac;
mod psa;
pub mod rand;
pub mod rsa;
pub mod tls;
pub mod x509;
#[cfg(test)]
mod tests;

pub use error::{CertError, Error, Result};
