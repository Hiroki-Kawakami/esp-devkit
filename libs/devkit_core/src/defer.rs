/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};

#[must_use]
pub struct Guard<T, F: FnOnce(T)> {
    value: ManuallyDrop<T>,
    f: ManuallyDrop<F>,
}

pub fn guard<T, F: FnOnce(T)>(value: T, f: F) -> Guard<T, F> {
    Guard { value: ManuallyDrop::new(value), f: ManuallyDrop::new(f) }
}

impl<T, F: FnOnce(T)> Guard<T, F> {
    pub fn into_inner(guard: Self) -> T {
        let mut guard = ManuallyDrop::new(guard);
        unsafe {
            ManuallyDrop::drop(&mut guard.f);
            ManuallyDrop::take(&mut guard.value)
        }
    }
}

impl<T, F: FnOnce(T)> Deref for Guard<T, F> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T, F: FnOnce(T)> DerefMut for Guard<T, F> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T, F: FnOnce(T)> Drop for Guard<T, F> {
    fn drop(&mut self) {
        let (value, f) = unsafe { (ManuallyDrop::take(&mut self.value), ManuallyDrop::take(&mut self.f)) };
        f(value);
    }
}

#[macro_export]
macro_rules! defer {
    ($($body:tt)*) => {
        let _guard = $crate::guard((), |()| {
            let _ = { $($body)* };
        });
    };
}

#[cfg(test)]
mod tests {
    use super::{Guard, guard};
    use std::cell::RefCell;

    #[test]
    fn runs_at_scope_end() {
        let log = RefCell::new(Vec::new());
        {
            let _g = guard(7, |v| log.borrow_mut().push(v));
            assert!(log.borrow().is_empty());
        }
        assert_eq!(*log.borrow(), [7]);
    }

    #[test]
    fn runs_on_early_return() {
        let log = RefCell::new(Vec::new());
        let open = |fail: bool| -> Result<i32, ()> {
            let a = guard(1, |v| log.borrow_mut().push(v));
            let b = guard(2, |v| log.borrow_mut().push(v));
            if fail {
                return Err(());
            }
            Ok(Guard::into_inner(a) + Guard::into_inner(b))
        };
        assert_eq!(open(true), Err(()));
        assert_eq!(*log.borrow(), [2, 1]);
        log.borrow_mut().clear();
        assert_eq!(open(false), Ok(3));
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn into_inner_drops_closure_captures() {
        let owned = std::rc::Rc::new(());
        let captured = owned.clone();
        let g = guard(5, move |_| drop(captured));
        assert_eq!(std::rc::Rc::strong_count(&owned), 2);
        assert_eq!(Guard::into_inner(g), 5);
        assert_eq!(std::rc::Rc::strong_count(&owned), 1);
    }

    #[test]
    fn deref_mut_reaches_the_closure() {
        let log = RefCell::new(Vec::new());
        {
            let mut g = guard(Vec::new(), |v: Vec<i32>| log.borrow_mut().extend(v));
            g.push(1);
            g.push(2);
            assert_eq!(g.len(), 2);
        }
        assert_eq!(*log.borrow(), [1, 2]);
    }

    #[test]
    fn defer_macro_runs_in_reverse_order() {
        let log = RefCell::new(Vec::new());
        {
            crate::defer! { log.borrow_mut().push(1) }
            crate::defer! {
                log.borrow_mut().push(2);
                log.borrow_mut().len()
            }
            log.borrow_mut().push(0);
        }
        assert_eq!(*log.borrow(), [0, 2, 1]);
    }
}
