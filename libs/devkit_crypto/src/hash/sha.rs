/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use super::Hash;
use crate::psa::{self, sys};

// mbedTLS does not promise that an operation survives being moved.
struct Op(Box<sys::psa_hash_operation_t>);

impl Op {
    fn new(alg: u32) -> Self {
        psa::init();
        let mut op = Op(unsafe { Box::new_zeroed().assume_init() });
        psa::check(unsafe { sys::psa_hash_setup(&mut *op.0, alg) });
        op
    }

    fn update(&mut self, data: &[u8]) {
        psa::check(unsafe { sys::psa_hash_update(&mut *self.0, data.as_ptr(), data.len()) });
    }

    fn finish(mut self, out: &mut [u8]) {
        let mut len = 0;
        psa::check(unsafe { sys::psa_hash_finish(&mut *self.0, out.as_mut_ptr(), out.len(), &mut len) });
        debug_assert_eq!(len, out.len());
    }
}

impl Clone for Op {
    fn clone(&self) -> Self {
        let mut op = Op(unsafe { Box::new_zeroed().assume_init() });
        psa::check(unsafe { sys::psa_hash_clone(&*self.0, &mut *op.0) });
        op
    }
}

impl Drop for Op {
    fn drop(&mut self) {
        unsafe { sys::psa_hash_abort(&mut *self.0) };
    }
}

macro_rules! sha {
    ($name:ident, $alg:expr, $out:expr, $block:expr) => {
        #[derive(Clone)]
        pub struct $name(Op);

        impl Hash for $name {
            const BLOCK: usize = $block;
            type Output = [u8; $out];
            const ZERO: [u8; $out] = [0; $out];

            fn new() -> Self {
                Self(Op::new($alg))
            }

            fn update(&mut self, data: &[u8]) {
                self.0.update(data);
            }

            fn finalize(self) -> [u8; $out] {
                let mut out = [0; $out];
                self.0.finish(&mut out);
                out
            }
        }
    };
}

sha!(Sha1, psa::ALG_SHA_1, 20, 64);
sha!(Sha256, psa::ALG_SHA_256, 32, 64);
sha!(Sha384, psa::ALG_SHA_384, 48, 128);
sha!(Sha512, psa::ALG_SHA_512, 64, 128);
