/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::time::Duration;

use bsp_rs::display::{self, Display, PixelFormat};
use devkit_gfx::Color;
use devkit_ui::widgets::{button, top_bar};
use devkit_ui::{Component, Cx, Element, ExternalSurface, Policy, Screen, Ui};
use idf::sync::OnceLock;
use idf::thread;

static DISPLAY: OnceLock<&'static Display> = OnceLock::new();
pub static VIDEO_FRAMES: AtomicU32 = AtomicU32::new(0);
const BAND_ROWS: usize = 64;
const BAR_WIDTH: usize = 90;
const BARS: [[u8; 3]; 4] = [[0xE5, 0x39, 0x35], [0x43, 0xA0, 0x47], [0x1E, 0x88, 0xE5], [0xFD, 0xD8, 0x35]];

pub fn set_display(display: &'static Display) {
    let _ = DISPLAY.set(display);
}

#[derive(Default)]
struct Playback {
    stop: AtomicBool,
    paused: AtomicBool,
    refresh: AtomicBool,
}

fn write_row(row: &mut [u8], format: PixelFormat, shift: usize) {
    let bytes = format.bytes();
    for (x, pixel) in row.chunks_exact_mut(bytes).enumerate() {
        let [r, g, b] = BARS[(x + shift) / BAR_WIDTH % BARS.len()];
        if bytes == 2 {
            let packed = u16::from(r >> 3) << 11 | u16::from(g >> 2) << 5 | u16::from(b >> 3);
            pixel.copy_from_slice(&packed.to_le_bytes());
        } else {
            pixel.copy_from_slice(&[b, g, r]);
        }
    }
}

fn produce(display: &'static Display, video: ExternalSurface, playback: Arc<Playback>) {
    let Ok(mut blitter) = display.blitter(1) else { return };
    let size = display.size();
    let mut storage = alloc::vec![0u8; usize::from(size.width.max(size.height)) * BAND_ROWS * 3 + 64];
    let start = storage.as_ptr().align_offset(64);
    let pixels = &mut storage[start..];
    let mut shift = 0;
    while !playback.stop.load(Ordering::Acquire) {
        let paused = playback.paused.load(Ordering::Acquire);
        if paused && !playback.refresh.swap(false, Ordering::AcqRel) {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        let Some(frame) = video.begin() else {
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        let area = frame.area;
        let format = frame.fb.format();
        let stride = usize::from(area.width) * format.bytes();
        let (first, rest) = pixels.split_at_mut(stride);
        write_row(first, format, shift);
        for row in rest[..stride * (BAND_ROWS - 1)].chunks_exact_mut(stride) {
            row.copy_from_slice(first);
        }
        let mut y = 0;
        while y < area.height {
            let height = (area.height - y).min(BAND_ROWS as u16);
            let band = display::Rect { x: area.x, y: area.y + y, width: area.width, height };
            if let Err(err) = unsafe { blitter.start(&frame.fb, frame.rotation, band, pixels.as_ptr()) } {
                idf::log::error!(c"ui_demo", "video: {err}");
                break;
            }
            blitter.wait_timeout(Duration::from_secs(1));
            y += height;
        }
        frame.end();
        VIDEO_FRAMES.fetch_add(1, Ordering::Relaxed);
        if !paused {
            shift = (shift + 3) % (BAR_WIDTH * BARS.len());
        }
    }
}

#[derive(Clone, Copy)]
pub enum VideoMsg {
    Back,
    Pause,
    Menu,
    Policy(Policy),
}

pub struct VideoDemo {
    video: ExternalSurface,
    playback: Arc<Playback>,
    menu: bool,
}

impl VideoDemo {
    pub(crate) fn new() -> Self {
        let video = ExternalSurface::new().expect("surface");
        let playback = Arc::new(Playback::default());
        let refresh = playback.clone();
        video.set_on_refresh(move |_| refresh.refresh.store(true, Ordering::Release));
        if let Some(&display) = DISPLAY.get() {
            let (video, playback) = (video.clone(), playback.clone());
            let spawned = thread::Builder::new().name("video").stack_size(8192).priority(5).spawn(move || produce(display, video, playback));
            if spawned.is_err() {
                idf::log::error!(c"ui_demo", "video task");
            }
        }
        Self { video, playback, menu: false }
    }
}

impl Drop for VideoDemo {
    fn drop(&mut self) {
        self.playback.stop.store(true, Ordering::Release);
    }
}

impl Component for VideoDemo {
    type Msg = VideoMsg;

    fn update(&mut self, msg: VideoMsg, cx: &mut Cx<Self>) {
        match msg {
            VideoMsg::Back => cx.nav().pop(),
            VideoMsg::Pause => {
                self.playback.paused.fetch_xor(true, Ordering::AcqRel);
            }
            VideoMsg::Menu => self.menu = !self.menu,
            VideoMsg::Policy(policy) => self.video.set_policy(policy),
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<VideoMsg>) -> Element<'a, VideoMsg> {
        let theme = ui.theme();
        let look = theme.looks.button_secondary;
        let menu = if self.menu {
            ui.column([ui.text(theme.fonts.title, "Over the video").color(Color::rgb(255, 255, 255))])
                .padding(theme.metrics.padding)
                .radius(theme.metrics.radius)
                .background(Color::rgba(0x20, 0x20, 0x28, 0xC0))
                .absolute(Some(80), Some(400), Some(80), None)
                .height(200)
                .id("video-menu")
        } else {
            ui.empty()
        };
        ui.column([
            top_bar(ui, Some(VideoMsg::Back), "Video", ui.empty()),
            ui.external(&self.video).grow(1).id("video"),
            ui.row([
                button(ui, "Pause", look, VideoMsg::Pause).grow(1).id("video-pause"),
                button(ui, "Menu", look, VideoMsg::Menu).grow(1).id("video-toggle"),
                button(ui, "Clip", look, VideoMsg::Policy(Policy::Clip)).grow(1).id("video-clip"),
                button(ui, "Stamp", look, VideoMsg::Policy(Policy::Stamp)).grow(1).id("video-stamp"),
                button(ui, "Full", look, VideoMsg::Policy(Policy::Exclusive)).grow(1).id("video-full"),
            ])
            .padding(theme.metrics.padding)
            .gap(theme.metrics.gap),
            menu,
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(ui.colors().surface)
    }
}

impl Screen for VideoDemo {}
