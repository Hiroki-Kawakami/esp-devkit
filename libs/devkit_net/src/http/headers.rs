/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;
use alloc::vec::Vec;

/// Header fields in the order they came, names matched without case.
#[derive(Clone, Default, Debug)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// The first field named `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.0.iter().filter(move |(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Adds a field; fields of the same name already there stay.
    pub fn append(&mut self, name: &str, value: &str) {
        self.0.push((String::from(name), String::from(value)));
    }

    pub fn remove(&mut self, name: &str) {
        self.0.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    pub(crate) fn last_mut(&mut self) -> Option<&mut String> {
        self.0.last_mut().map(|(_, v)| v)
    }
}
