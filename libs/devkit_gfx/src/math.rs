/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::f32::consts::{FRAC_PI_2, PI, TAU};

pub(crate) fn sqrt(x: f32) -> f32 {
    if x <= 0.0 { 0.0 } else { core::intrinsics::sqrtf32(x) }
}

// `core` has none of these.

fn atan_unit(z: f32) -> f32 {
    let z2 = z * z;
    z * (0.999_866 + z2 * (-0.330_299_5 + z2 * (0.180_141 + z2 * (-0.085_133 + z2 * 0.020_835_1))))
}

/// Of a turn, clockwise from straight up on a screen whose y grows downward.
pub(crate) fn turns(dx: f32, dy: f32) -> f32 {
    let (y, x) = (dx, -dy);
    if x == 0.0 && y == 0.0 {
        return 0.0;
    }
    let angle = if x.abs() >= y.abs() {
        let a = atan_unit(y / x);
        if x > 0.0 { a } else if y >= 0.0 { a + PI } else { a - PI }
    } else {
        let a = atan_unit(x / y);
        if y > 0.0 { FRAC_PI_2 - a } else { -FRAC_PI_2 - a }
    };
    let t = angle / TAU;
    if t < 0.0 { t + 1.0 } else { t }
}

/// `t` of a turn, clockwise from straight up on a screen whose y grows
/// downward, as the step `(dx, dy)` of length 1.
pub(crate) fn direction(t: f32) -> (f32, f32) {
    let t = t - t as i32 as f32;
    let t = if t < 0.0 { t + 1.0 } else { t };
    let a = t * TAU - PI;
    let (sin, cos) = sin_cos(a);
    (-sin, cos)
}

/// For `a` in -π..π.
fn sin_cos(a: f32) -> (f32, f32) {
    let (fold, sign) = if a > FRAC_PI_2 { (PI - a, -1.0) } else if a < -FRAC_PI_2 { (-PI - a, -1.0) } else { (a, 1.0) };
    let a2 = fold * fold;
    let sin = fold * (1.0 - a2 / 6.0 * (1.0 - a2 / 20.0 * (1.0 - a2 / 42.0 * (1.0 - a2 / 72.0))));
    let cos = 1.0 - a2 / 2.0 * (1.0 - a2 / 12.0 * (1.0 - a2 / 30.0 * (1.0 - a2 / 56.0 * (1.0 - a2 / 90.0))));
    (sin, sign * cos)
}
