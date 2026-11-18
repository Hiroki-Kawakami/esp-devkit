/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[doc(hidden)]
pub use alloc::boxed::Box as __Box;

/// `Box::new(Type { field: value, .. })` written straight into the heap, so the
/// whole value never sits on the stack. Like a struct expression, every field
/// must be given once.
///
/// A value that returns early with `?` or panics leaks the fields written
/// before it, so do fallible work before the macro.
#[macro_export]
macro_rules! boxed {
    (@value $field:ident) => { $field };
    (@value $field:ident: $value:expr) => { $value };
    ($ty:ident { $($field:ident $(: $value:expr)?),* $(,)? }) => {{
        let mut slot = $crate::__Box::<$ty>::new_uninit();
        let ptr = slot.as_mut_ptr();
        $(unsafe { ::core::ptr::addr_of_mut!((*ptr).$field).write($crate::boxed!(@value $field $(: $value)?)) };)*
        // Never runs; it fails to compile when a field is missing or repeated.
        #[allow(unreachable_code, clippy::diverging_sub_expression)]
        if false {
            let _ = $ty { $($field: loop {}),* };
        }
        unsafe { slot.assume_init() }
    }};
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    struct Pair {
        name: &'static str,
        count: Rc<()>,
        values: [u32; 64],
    }

    impl Pair {
        fn new(count: Rc<()>) -> Box<Self> {
            crate::boxed!(Self { name: "pair", count, values: [7; 64], })
        }
    }

    #[test]
    fn fills_every_field() {
        let pair = Pair::new(Rc::new(()));
        assert_eq!(pair.name, "pair");
        assert!(pair.values.iter().all(|&v| v == 7));
    }

    #[test]
    fn owns_the_values_written() {
        let count = Rc::new(());
        let pair = Pair::new(count.clone());
        assert_eq!(Rc::strong_count(&count), 2);
        drop(pair);
        assert_eq!(Rc::strong_count(&count), 1);
    }
}
