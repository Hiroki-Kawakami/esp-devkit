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
mod nav;
mod node;
mod paint;
mod presenter;
mod style;
mod theme;
mod tree;
pub mod widgets;

#[cfg(test)]
mod tests;

pub use app::{App, AppConfig, Handle, Probe};
pub use bsp_rs::display::Rotation;
pub use component::{Animation, Child, Component, Cx, ListId, Nav, Property, RenderMode, Screen};
pub use element::{ChildElement, Children, Drag, Element, Ui};
pub use nav::{Overlay, Root};
pub use presenter::{Config, PresentMode, Presenter, Stats, StripMemory, Strips};
pub use style::{Align, Bars, Dimension, Direction, Insets, Justify, Role, ScreenStyle, Shadow, Style};
pub use theme::{Fonts, Metrics, Motion, Palette, Theme, Transition, WidgetStyle};
