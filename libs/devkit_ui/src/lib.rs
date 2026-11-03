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
pub mod keyboard;
mod layout;
mod motion;
mod nav;
mod node;
mod paint;
mod plugin;
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
pub use canvas::{Canvas, CanvasId};
pub use component::{Animation, Child, Component, Cx, ListId, Nav, Property, RenderMode, Screen};
pub use devkit_gfx::{ImageFormat, Raster, RasterTarget, Rect};
pub use element::{ChildElement, Children, Drag, Element, Ui};
pub use external::{ExternalFrame, ExternalSurface, Policy};
pub use gesture::{Direction as SwipeDirection, Gesture, Gestures, Phase};
pub use keyboard::KeyRole;
pub use nav::{Overlay, Root, SplitRole};
pub use rotation::{AutoRotation, Rotations};
pub use plugin::{Plugin, PluginContext, PluginInit, SavedColorMode, Store, UiEvent};
pub use presence::{Edge, Presence};
pub use presenter::{Config, PresentMode, Presenter, Stats, StripMemory, Strips};
pub use style::{Align, Animated, Bars, Dimension, Direction, Insets, Justify, Part, Prop, Role, ScreenStyle, Shadow, State, StateStyle, Style};
pub use text_buffer::{TextBuffer, TextInput};
pub use theme::{ButtonTheme, CheckboxTheme, ColorMode, ColorRole, DropdownMenuTheme, DropdownTheme, Fonts, Icon, IconButtonTheme, Icons, KeyboardTheme, Keyboards, ListItemTheme, ListTheme, MenuItemTheme, MenuTheme, ModalTheme, Motion, OverlayTheme, Palette, ProgressBarTheme, ProgressIndicatorTheme, SegmentedTheme, SliderTheme, SwitchTheme, TableHeaderTheme, TableRowTheme, TableSectionTheme, TableTheme, TextFieldTheme, Theme, ToastTheme, TopBarTheme, Transition, UnitTheme, Widgets};
