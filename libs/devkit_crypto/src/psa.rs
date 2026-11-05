/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Error, Result};

pub(crate) mod sys {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

pub(crate) const ALG_SHA_1: u32 = 0x0200_0005;
pub(crate) const ALG_SHA_256: u32 = 0x0200_0009;
pub(crate) const ALG_SHA_384: u32 = 0x0200_000a;
pub(crate) const ALG_SHA_512: u32 = 0x0200_000b;
pub(crate) const ALG_ANY_HASH: u32 = 0x0200_00ff;
pub(crate) const ALG_GCM: u32 = 0x0550_0200;
pub(crate) const ALG_CHACHA20_POLY1305: u32 = 0x0510_0500;
pub(crate) const ALG_ECDH: u32 = 0x0902_0000;
pub(crate) const ALG_ECDSA_BASE: u32 = 0x0600_0600;
pub(crate) const ALG_RSA_PKCS1V15_SIGN_BASE: u32 = 0x0600_0200;
pub(crate) const ALG_RSA_PSS_BASE: u32 = 0x0600_0300;

pub(crate) const KEY_TYPE_AES: u16 = 0x2400;
pub(crate) const KEY_TYPE_CHACHA20: u16 = 0x2004;
pub(crate) const KEY_TYPE_RSA_PUBLIC_KEY: u16 = 0x4001;
pub(crate) const KEY_TYPE_RSA_KEY_PAIR: u16 = 0x7001;
pub(crate) const KEY_TYPE_ECC_PUBLIC_KEY_BASE: u16 = 0x4100;
pub(crate) const KEY_TYPE_ECC_KEY_PAIR_BASE: u16 = 0x7100;
pub(crate) const ECC_FAMILY_SECP_R1: u16 = 0x12;
pub(crate) const ECC_FAMILY_MONTGOMERY: u16 = 0x41;

pub(crate) const USAGE_EXPORT: u32 = 0x0001;
pub(crate) const USAGE_ENCRYPT: u32 = 0x0100;
pub(crate) const USAGE_DECRYPT: u32 = 0x0200;
pub(crate) const USAGE_SIGN_MESSAGE: u32 = 0x0400;
pub(crate) const USAGE_VERIFY_MESSAGE: u32 = 0x0800;
pub(crate) const USAGE_SIGN_HASH: u32 = 0x1000;
pub(crate) const USAGE_VERIFY_HASH: u32 = 0x2000;
pub(crate) const USAGE_DERIVE: u32 = 0x4000;

const ERROR_NOT_SUPPORTED: i32 = -134;
const ERROR_INVALID_ARGUMENT: i32 = -135;
const ERROR_BUFFER_TOO_SMALL: i32 = -138;
const ERROR_INSUFFICIENT_MEMORY: i32 = -141;
const ERROR_INVALID_SIGNATURE: i32 = -149;

static READY: AtomicBool = AtomicBool::new(false);

pub(crate) fn init() {
    if !READY.load(Ordering::Acquire) {
        check(unsafe { sys::psa_crypto_init() });
        READY.store(true, Ordering::Release);
    }
}

#[track_caller]
pub(crate) fn check(status: sys::psa_status_t) {
    if status != 0 {
        panic!("devkit_crypto: PSA error {status}");
    }
}

pub(crate) fn result(status: sys::psa_status_t) -> Result<()> {
    match status {
        0 => Ok(()),
        ERROR_INVALID_SIGNATURE => Err(Error::Verify),
        ERROR_INVALID_ARGUMENT => Err(Error::InvalidKey),
        ERROR_NOT_SUPPORTED => Err(Error::Unsupported),
        ERROR_BUFFER_TOO_SMALL => Err(Error::InvalidLength),
        ERROR_INSUFFICIENT_MEMORY => Err(Error::NoMemory),
        status => Err(Error::Psa(status)),
    }
}

pub(crate) struct Policy {
    pub ty: u16,
    pub bits: usize,
    pub usage: u32,
    pub alg: u32,
    pub alg2: u32,
}

// The psa_set_key_* setters are static inline, which host tests do not link.
fn attributes(policy: &Policy) -> sys::psa_key_attributes_t {
    let mut attr: sys::psa_key_attributes_t = unsafe { core::mem::zeroed() };
    attr.private_type = policy.ty;
    attr.private_bits = policy.bits as u16;
    let mut usage = policy.usage;
    if usage & USAGE_SIGN_HASH != 0 {
        usage |= USAGE_SIGN_MESSAGE;
    }
    if usage & USAGE_VERIFY_HASH != 0 {
        usage |= USAGE_VERIFY_MESSAGE;
    }
    attr.private_policy.private_usage = usage;
    attr.private_policy.private_alg = policy.alg;
    attr.private_policy.private_alg2 = policy.alg2;
    attr
}

/// A volatile key in PSA's key store, destroyed on drop.
pub(crate) struct Key(sys::mbedtls_svc_key_id_t);

impl Key {
    pub fn import(policy: &Policy, data: &[u8]) -> Result<Self> {
        init();
        let attr = attributes(policy);
        let mut id = 0;
        result(unsafe { sys::psa_import_key(&attr, data.as_ptr(), data.len(), &mut id) })?;
        Ok(Self(id))
    }

    pub fn generate(policy: &Policy) -> Result<Self> {
        init();
        let attr = attributes(policy);
        let mut id = 0;
        result(unsafe { sys::psa_generate_key(&attr, &mut id) })?;
        Ok(Self(id))
    }

    pub fn id(&self) -> sys::mbedtls_svc_key_id_t {
        self.0
    }

    pub fn export(&self, max: usize) -> Result<Vec<u8>> {
        let mut out = vec![0; max];
        let mut len = 0;
        result(unsafe { sys::psa_export_key(self.0, out.as_mut_ptr(), out.len(), &mut len) })?;
        out.truncate(len);
        Ok(out)
    }

    pub fn export_public(&self, max: usize) -> Result<Vec<u8>> {
        let mut out = vec![0; max];
        let mut len = 0;
        result(unsafe { sys::psa_export_public_key(self.0, out.as_mut_ptr(), out.len(), &mut len) })?;
        out.truncate(len);
        Ok(out)
    }

    pub fn sign_message(&self, alg: u32, message: &[u8], max: usize) -> Result<Vec<u8>> {
        let mut out = vec![0; max];
        let mut len = 0;
        result(unsafe {
            sys::psa_sign_message(self.0, alg, message.as_ptr(), message.len(), out.as_mut_ptr(), out.len(), &mut len)
        })?;
        out.truncate(len);
        Ok(out)
    }

    pub fn sign_hash(&self, alg: u32, hash: &[u8], max: usize) -> Result<Vec<u8>> {
        let mut out = vec![0; max];
        let mut len = 0;
        result(unsafe { sys::psa_sign_hash(self.0, alg, hash.as_ptr(), hash.len(), out.as_mut_ptr(), out.len(), &mut len) })?;
        out.truncate(len);
        Ok(out)
    }

    pub fn verify_message(&self, alg: u32, message: &[u8], signature: &[u8]) -> Result<()> {
        result(unsafe {
            sys::psa_verify_message(self.0, alg, message.as_ptr(), message.len(), signature.as_ptr(), signature.len())
        })
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { sys::psa_destroy_key(self.0) };
    }
}
