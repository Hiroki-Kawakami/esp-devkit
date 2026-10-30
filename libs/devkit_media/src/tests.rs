/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

mod buffer;
mod differential;

use std::ffi::CString;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::{ARENA_ALIGNMENT, Arena};

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Line([u8; ARENA_ALIGNMENT]);

struct Memory(Vec<Line>);

impl Memory {
    fn new(bytes: usize) -> Self {
        Self(vec![Line([0; ARENA_ALIGNMENT]); bytes / ARENA_ALIGNMENT])
    }
}

impl Deref for Memory {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast(), self.0.len() * ARENA_ALIGNMENT) }
    }
}

impl DerefMut for Memory {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.0.as_mut_ptr().cast(), self.0.len() * ARENA_ALIGNMENT) }
    }
}

fn arena(bytes: usize) -> Arena {
    Arena::new(Memory::new(bytes))
}

fn c_path(path: &Path) -> CString {
    CString::new(path.to_str().unwrap()).unwrap()
}

fn bounded<T: Send + 'static>(seconds: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(seconds)).expect("deadlock: no result in time")
}
