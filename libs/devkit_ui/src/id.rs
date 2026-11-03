/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

/// Names a node: it matches across rebuilds by this among its siblings, and
/// `cx.focus`, `cx.animate`, `ui tap` and the like find it by this. Ids that
/// differ make another node.
#[derive(Clone, Copy, Debug)]
pub struct Id {
    hash: u32,
    name: &'static str,
}

impl Id {
    /// What `id!` makes; `name` shows in `ui tree`.
    pub const fn named(name: &'static str) -> Self {
        Self { hash: hash(name), name }
    }

    /// For a name made at run time, which `ui tree` cannot show.
    pub fn of(name: &str) -> Self {
        Self { hash: hash(name), name: "" }
    }

    pub(crate) fn name(&self) -> Option<&'static str> {
        (!self.name.is_empty()).then_some(self.name)
    }

    pub(crate) fn hash(&self) -> u32 {
        self.hash
    }
}

impl PartialEq for Id {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

impl Eq for Id {}

impl From<u32> for Id {
    fn from(value: u32) -> Self {
        Self { hash: value, name: "" }
    }
}

const fn hash(name: &str) -> u32 {
    let bytes = name.as_bytes();
    let mut hash = 0x811c_9dc5u32;
    let mut i = 0;
    while i < bytes.len() {
        hash = (hash ^ bytes[i] as u32).wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash
}

/// An `Id` from a string literal, hashed at compile time.
#[macro_export]
macro_rules! id {
    ($name:literal) => {
        const { $crate::Id::named($name) }
    };
}
