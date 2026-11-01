/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit_ui::widgets;
use devkit_ui::{Icon, Strips, Theme};
use idf::sync::{LazyLock, Mutex};

use crate::resources::TOUCH;
use crate::scene::{ICONS, SMALL_ICONS};

mod canvas;
mod files;
mod gestures;
mod home;
mod settings;
mod status_bar;
mod video;

pub use files::set_card_present;
pub use home::Home;
pub use settings::{set_input, set_rotation};
pub use status_bar::{StatusBar, StatusMsg};
pub use video::{VIDEO_FRAMES, set_display};

const fn icon(glyph: char) -> Icon {
    Icon { font: &ICONS, glyph }
}
const fn small_icon(glyph: char) -> Icon {
    Icon { font: &SMALL_ICONS, glyph }
}
pub static THEME: Theme = Theme::new(&TOUCH, &widgets::DEFAULT);

/// The strip memory not in use.
pub static SPARE_STRIPS: LazyLock<Mutex<Option<Strips>>> = LazyLock::new(|| Mutex::new(None));

pub const INSET_TOP: i16 = 52;
