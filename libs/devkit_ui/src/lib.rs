/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![cfg_attr(not(test), no_std)]
#![feature(associated_type_defaults)]

extern crate alloc;

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
mod nav;
mod node;
mod paint;
mod plugin;
mod presenter;
mod rotation;
mod style;
mod text_buffer;
mod theme;
mod tree;
pub mod widgets;

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
pub use nav::{Overlay, Root, SplitRole};
pub use rotation::{AutoRotation, Rotations};
pub use plugin::{Plugin, PluginContext, PluginInit, SavedColorMode, Store, UiEvent};
pub use presenter::{Config, PresentMode, Presenter, Stats, StripMemory, Strips};
pub use style::{Align, Bars, Dimension, Direction, Insets, Justify, Part, Role, ScreenStyle, Shadow, State, Style};
pub use text_buffer::{TextBuffer, TextInput};
pub use theme::{Border, ButtonTheme, CheckboxTheme, ColorMode, ColorRole, DropdownMenuTheme, DropdownTheme, Fonts, Icon, IconButtonTheme, Icons, KeyboardTheme, Keyboards, ListItemTheme, Look, LookBorder, MenuItemTheme, MenuTheme, ModalTheme, Motion, Palette, SegmentedTheme, SliderTheme, SwitchTheme, TableHeaderTheme, TableRowTheme, TableSectionTheme, TableTheme, TextFieldTheme, Theme, ThemeAssets, ToastTheme, TopBarTheme, Transition, UnitTheme, WidgetLook, WidgetStyle, Widgets};
