/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
#![feature(associated_type_defaults)]

extern crate alloc;

mod app;
mod bump;
mod component;
mod element;
mod erased;
mod layout;
mod node;
mod paint;
mod presenter;
mod style;
mod tree;

#[cfg(test)]
mod tests;

pub use app::{App, AppConfig, Handle, Probe};
pub use bsp_rs::display::Rotation;
pub use component::{Child, Component, Cx, ListId};
pub use element::{ChildElement, Children, Element, Ui};
pub use presenter::{Config, PresentMode, Presenter, Stats, StripMemory, Strips};
pub use style::{Align, Direction, Dimension, Justify, Role, Shadow, Style};
