/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ops::{Deref, DerefMut};

use crate::Zeroable;

/// `T` on a 16-byte boundary, as vector kernels take their operands.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
#[repr(C, align(16))]
pub struct A16<T>(pub T);

impl<T> Deref for A16<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for A16<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

unsafe impl<T: Zeroable> Zeroable for A16<T> {}

#[cfg(test)]
mod tests {
    use super::A16;

    #[test]
    fn aligns_any_payload() {
        let values = [A16(1u8), A16(2u8)];
        assert_eq!(values[0..].as_ptr() as usize % 16, 0);
        assert_eq!(core::mem::size_of::<A16<[i16; 9]>>(), 32);
        assert_eq!(values[1].wrapping_add(1), 3);
    }
}
