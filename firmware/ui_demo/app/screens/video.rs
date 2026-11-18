/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::time::Duration;

use bsp_rs::display::{Display, ImageFormat, Rect};
use devkit_gfx::Color;
use devkit_ui::widgets::{button, top_bar};
use devkit_ui::{Component, Cx, Element, ExternalFrame, ExternalSurface, Policy, Screen, Ui, id};
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

fn write_row(row: &mut [u8], format: ImageFormat, shift: usize) {
    let bytes = format.bits() / 8;
    for (x, pixel) in row.chunks_exact_mut(bytes).enumerate() {
        let [r, g, b] = BARS[(x + shift) / BAR_WIDTH % BARS.len()];
        if bytes == 2 {
            let packed = u16::from(r >> 3) << 11 | u16::from(g >> 2) << 5 | u16::from(b >> 3);
            let packed = if format == ImageFormat::Rgb565Be { packed.to_be_bytes() } else { packed.to_le_bytes() };
            pixel.copy_from_slice(&packed);
        } else {
            pixel.copy_from_slice(&[b, g, r]);
        }
    }
}

/// `band` rows of `pixels`, the same for every band, down the frame through the
/// 2D engine.
#[cfg(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED))]
fn blit_bands(frame: &ExternalFrame, pixels: &[u8], band: usize) -> idf::Result<()> {
    use devkit_gfx::Image;
    use media_engine::{Slot, Srm};
    let (ppa, mut slot) = (media_engine::ppa()?, Slot::new());
    let (area, format) = (frame.area, frame.format());
    let stride = format.row_bytes(area.w as usize);
    let mut y = 0;
    while y < area.h {
        let height = (area.h - y).min(band as i16);
        let image = Image::from_pixels(format, area.w as u16, height as u16, area.w as usize, &pixels[..stride * height as usize]);
        let mut op = Srm::new(image);
        op.at = (area.x, area.y + y);
        let mut target = frame.target().and_then(|t| t.crop(Rect::new(area.x, area.y + y, area.w, height))).ok_or(idf::Error::INVALID_ARG)?;
        ppa.srm(&op, &mut target, &mut slot)?.wait()?;
        y += height;
    }
    Ok(())
}

#[cfg(not(any(not(target_os = "none"), CONFIG_SOC_PPA_SUPPORTED)))]
fn blit_bands(_frame: &ExternalFrame, _pixels: &[u8], _band: usize) -> idf::Result<()> {
    Err(idf::Error::NOT_SUPPORTED)
}

/// The same, written to a panel without framebuffers; returns once sent.
fn write_bands(frame: &ExternalFrame, pixels: &[u8], band: usize) -> idf::Result<()> {
    let area = frame.area;
    let stride = frame.format().row_bytes(area.w as usize);
    let (mut y, mut last) = (0, None);
    while y < area.h {
        let height = (area.h - y).min(band as i16);
        last = Some(unsafe { frame.write(Rect::new(area.x, area.y + y, area.w, height), &pixels[..stride * height as usize]) }?);
        y += height;
    }
    last.map_or(Ok(()), |ticket| frame.wait(ticket))
}

fn produce(display: &'static Display, video: ExternalSurface, playback: Arc<Playback>) {
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
        let format = frame.format();
        let stride = format.row_bytes(area.w as usize);
        let (first, rest) = pixels.split_at_mut(stride);
        write_row(first, format, shift);
        for row in rest[..stride * (BAND_ROWS - 1)].chunks_exact_mut(stride) {
            row.copy_from_slice(first);
        }
        let sent = if frame.fb.is_some() { blit_bands(&frame, pixels, BAND_ROWS) } else { write_bands(&frame, pixels, BAND_ROWS) };
        if let Err(err) = sent {
            idf::log::error!(c"ui_demo", "video: {err}");
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
        let style = theme.widgets.button.secondary;
        let (width, height) = ui.screen_size();
        let (inset, top, tall) = if width.min(height) < 480 { (16, 96, 56) } else { (80, 400, 200) };
        let menu = if self.menu {
            ui.column([ui.text(theme.fonts.title, "Over the video").foreground(Color::rgb(255, 255, 255))])
                .padding(theme.widgets.unit.padding)
                .radius(theme.widgets.unit.radius)
                .background(Color::rgba(0x20, 0x20, 0x28, 0xC0))
                .absolute(Some(inset), Some(top), Some(inset), None)
                .height(tall)
                .id(id!("video-menu"))
        } else {
            ui.empty()
        };
        ui.column([
            top_bar(ui, Some(VideoMsg::Back), "Video", ui.empty()),
            ui.external(&self.video).grow(1).id(id!("video")),
            ui.row([
                button(ui, "Pause", style, VideoMsg::Pause).grow(1).id(id!("video-pause")),
                button(ui, "Menu", style, VideoMsg::Menu).grow(1).id(id!("video-toggle")),
                button(ui, "Clip", style, VideoMsg::Policy(Policy::Clip)).grow(1).id(id!("video-clip")),
                button(ui, "Stamp", style, VideoMsg::Policy(Policy::Stamp)).grow(1).id(id!("video-stamp")),
                button(ui, "Full", style, VideoMsg::Policy(Policy::Exclusive)).grow(1).id(id!("video-full")),
            ])
            .padding(theme.widgets.unit.padding)
            .gap(theme.widgets.unit.gap),
            menu,
        ])
        .padding_each(0, ui.insets().top, 0, 0)
        .background(ui.colors().surface)
    }
}

impl Screen for VideoDemo {}
