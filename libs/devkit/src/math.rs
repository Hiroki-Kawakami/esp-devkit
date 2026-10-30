/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::f32::math as f32m;
use core::f64::math as f64m;
use core::ffi::{c_int, c_long};

mod libm {
    use super::{c_int, c_long};

    unsafe extern "C" {
        pub safe fn sinf(x: f32) -> f32;
        pub safe fn cosf(x: f32) -> f32;
        pub safe fn powf(x: f32, y: f32) -> f32;
        pub safe fn log2f(x: f32) -> f32;
        pub safe fn ldexpf(x: f32, exp: c_int) -> f32;
        pub safe fn lroundf(x: f32) -> c_long;
        pub safe fn sin(x: f64) -> f64;
        pub safe fn cos(x: f64) -> f64;
        pub safe fn pow(x: f64, y: f64) -> f64;
        pub safe fn log2(x: f64) -> f64;
        pub safe fn atan2(y: f64, x: f64) -> f64;
        pub safe fn lround(x: f64) -> c_long;
        #[cfg(not(all(target_arch = "riscv32", target_feature = "f")))]
        pub safe fn lrintf(x: f32) -> c_long;
    }
}

pub use libm::{atan2, cos, cosf, log2, log2f, pow, powf, sin, sinf};

#[inline]
pub fn ldexpf(x: f32, exp: i32) -> f32 {
    libm::ldexpf(x, exp)
}

#[inline]
pub fn lroundf(x: f32) -> i32 {
    libm::lroundf(x) as i32
}

#[inline]
pub fn lround(x: f64) -> i32 {
    libm::lround(x) as i32
}

// With the FPU's rounding mode, as C's -fno-math-errno gets it. Rust has no
// lrint intrinsic, and a libm call per sample would cost far more.
#[cfg(all(target_arch = "riscv32", target_feature = "f"))]
#[inline]
pub fn lrintf(x: f32) -> i32 {
    let r: i32;
    unsafe {
        core::arch::asm!("fcvt.w.s {r}, {x}", r = lateout(reg) r, x = in(freg) x,
                         options(pure, nomem, nostack));
    }
    r
}

#[cfg(not(all(target_arch = "riscv32", target_feature = "f")))]
#[inline]
pub fn lrintf(x: f32) -> i32 {
    libm::lrintf(x) as i32
}

#[inline]
pub fn sqrtf(x: f32) -> f32 {
    f32m::sqrt(x)
}

#[inline]
pub fn floorf(x: f32) -> f32 {
    f32m::floor(x)
}

#[inline]
pub fn ceilf(x: f32) -> f32 {
    f32m::ceil(x)
}

#[inline]
pub fn roundf(x: f32) -> f32 {
    f32m::round(x)
}

#[inline]
pub fn sqrt(x: f64) -> f64 {
    f64m::sqrt(x)
}

#[inline]
pub fn floor(x: f64) -> f64 {
    f64m::floor(x)
}

#[inline]
pub fn round(x: f64) -> f64 {
    f64m::round(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_follows_c() {
        assert_eq!(lrintf(2.5), 2);
        assert_eq!(lrintf(3.5), 4);
        assert_eq!(lrintf(-2.5), -2);
        assert_eq!(lroundf(2.5), 3);
        assert_eq!(lroundf(-2.5), -3);
        assert_eq!(lround(-3.5), -4);
        assert_eq!(roundf(2.5), 3.0);
        assert_eq!(round(-2.5), -3.0);
        assert_eq!(floorf(-1.5), -2.0);
        assert_eq!(ceilf(-1.5), -1.0);
        assert_eq!(floor(1.5), 1.0);
    }

    #[test]
    fn functions_match_std() {
        assert_eq!(sqrtf(2.0), 2f32.sqrt());
        assert_eq!(sqrt(2.0), 2f64.sqrt());
        assert_eq!(ldexpf(3.0, 4), 48.0);
        assert_eq!(powf(2.0, 10.0), 1024.0);
        assert_eq!(pow(2.0, -2.0), 0.25);
        assert_eq!(log2f(8.0), 3.0);
        assert_eq!(log2(1024.0), 10.0);
        assert!((sinf(1.0) - 1f32.sin()).abs() < 1e-6);
        assert!((cosf(1.0) - 1f32.cos()).abs() < 1e-6);
        assert!((sin(1.0) - 1f64.sin()).abs() < 1e-12);
        assert!((cos(1.0) - 1f64.cos()).abs() < 1e-12);
        assert!((atan2(1.0, 1.0) - core::f64::consts::FRAC_PI_4).abs() < 1e-12);
    }
}
