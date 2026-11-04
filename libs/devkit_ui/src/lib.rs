/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
#![feature(associated_type_defaults)]

extern crate alloc;

mod animate;
mod app;
mod bump;
mod canvas;
mod component;
mod element;
mod erased;
mod external;
mod gesture;
mod id;
mod inbox;
pub mod keyboard;
mod layout;
mod motion;
mod nav;
mod node;
mod paint;
mod plugin;
mod popover;
mod presence;
mod progress;
mod presenter;
mod rotation;
mod style;
mod text_buffer;
mod theme;
mod tree;
pub mod widgets;
mod zoom;

#[cfg(test)]
mod tests;

pub use app::{App, AppConfig, Handle, Input, Probe};
pub use bsp_rs::display::Rotation;
pub use canvas::Canvas;
pub use component::{Animation, Child, Component, Cx, Nav, Property, RenderMode, Screen, ScrollEdge, ScrollTarget};
pub use devkit_gfx::{Corners, Elide, ImageFormat, Raster, RasterTarget, Rect, Sides};
pub use element::{ChildElement, Children, CollectionLayout, Drag, Element, Ui};
pub use external::{ExternalFrame, ExternalSurface, Policy};
pub use gesture::{Direction as SwipeDirection, Gesture, Gestures, Phase};
pub use id::Id;
pub use inbox::{Inbox, Pending, Reply, Sender};
pub use keyboard::KeyRole;
pub use nav::{Overlay, Root, SplitRole};
pub use rotation::{AutoRotation, Rotations};
pub use plugin::{Plugin, PluginContext, PluginInit, SavedColorMode, Store, UiEvent};
pub use popover::{Placement, Popover, PopoverAlign};
pub use presence::{Edge, Presence};
pub use presenter::{Config, PresentMode, Presenter, Stats, StripMemory, Strips};
pub use style::{Align, Animated, Bars, Dimension, Direction, Insets, Justify, Part, Prop, Role, ScreenStyle, Shadow, State, StateStyle, Style};
pub use text_buffer::{TextBuffer, TextInput};
pub use theme::{ButtonTheme, CheckboxTheme, ColorMode, ColorRole, DividerTheme, DropdownMenuTheme, DropdownTheme, Fonts, Icon, IconButtonTheme, Icons, KeyboardTheme, Keyboards, ListItemTheme, MenuTheme, ModalTheme, Motion, OverlayTheme, Palette, ProgressBarTheme, ProgressIndicatorTheme, SegmentedTheme, SliderTheme, SwitchTheme, TableHeaderTheme, TableRowTheme, TableSectionTheme, TableTheme, TextFieldTheme, Theme, ToastTheme, TopBarTheme, Transition, UnitTheme, Widgets};
