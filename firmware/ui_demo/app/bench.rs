/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::time::Duration;

use bsp_rs::display::{Display, PixelFormat};
use devkit_gfx::{DisplayList, GlyphCache, ImageFormat};
use devkit_ui::{Config, PresentMode, Presenter, Rotation, Stats, Strips};
use idf::heap::Caps;
use idf::thread;
use idf::time::Instant;

use crate::scene::{Assets, Kind, Scene};

const GLYPH_CACHE_BYTES: usize = 128 * 1024;
const WARMUP_FRAMES: usize = 8;
const MAX_FRAME_PERIOD_US: u32 = 19_000;
const STRIP_BYTES: usize = 4 * 64 * 1024;
const LOAD_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Present {
    InPlace,
    Swap2,
    Swap3,
}

pub struct Params {
    pub kind: Kind,
    pub present: Present,
    pub rotated: bool,
    pub lanes: usize,
    pub frames: usize,
    pub speed: i32,
    pub strips_psram: bool,
    /// A task copying PSRAM on the other core, as a decoder would.
    pub load: bool,
}

pub struct Bench {
    presenter: Presenter,
    assets: [&'static Assets; 2],
    cache: GlyphCache<Caps>,
    list: DisplayList<'static, Caps>,
    spare: Option<Strips>,
    in_psram: bool,
    load_stop: Arc<AtomicBool>,
    load_bytes: Arc<AtomicU32>,
}

impl Bench {
    /// `sram` holds the strips unless a run asks for PSRAM.
    pub fn new(display: &'static Display, sram: Strips) -> idf::Result<Self> {
        let format = if display.pixel_format() == PixelFormat::Rgb888 { ImageFormat::Rgb888 } else { ImageFormat::Rgb565 };
        let size = display.size();
        let assets = [
            &*Box::leak(Box::new(Assets::new(format, size.width as i16))),
            &*Box::leak(Box::new(Assets::new(format, size.height as i16))),
        ];
        let config = Config { strip_bytes: STRIP_BYTES, strip_caps: Caps::SPIRAM, ..Config::default() };
        let mut presenter = Presenter::new(display, &config)?;
        let spare = presenter.set_strips(sram, 2).map_err(|_| idf::Error::INVALID_SIZE)?;
        Ok(Self {
            presenter,
            assets,
            cache: GlyphCache::new_in(GLYPH_CACHE_BYTES, Caps::SPIRAM),
            list: DisplayList::new_in(Caps::INTERNAL),
            spare: Some(spare),
            in_psram: false,
            load_stop: Arc::new(AtomicBool::new(true)),
            load_bytes: Arc::new(AtomicU32::new(0)),
        })
    }

    fn setup(&mut self, p: &Params) -> idf::Result<Scene> {
        let (mode, count) = match p.present {
            Present::InPlace => (PresentMode::InPlace, 1),
            Present::Swap2 => (PresentMode::Swap, 2),
            Present::Swap3 => (PresentMode::Swap, 3),
        };
        let rotation = if p.rotated { Rotation::R90 } else { Rotation::R0 };
        self.presenter.configure(mode, count, rotation, p.lanes)?;
        if self.in_psram != p.strips_psram {
            let strips = self.spare.take().ok_or(idf::Error::INVALID_STATE)?;
            match self.presenter.set_strips(strips, p.lanes) {
                Ok(old) => self.spare = Some(old),
                Err(strips) => {
                    self.spare = Some(strips);
                    return Err(idf::Error::INVALID_SIZE);
                }
            }
            self.in_psram = p.strips_psram;
        }
        let area = self.presenter.area();
        Ok(Scene { kind: p.kind, width: area.w, height: area.h, offset: 0, speed: p.speed })
    }

    fn frame(&mut self, scene: &mut Scene) -> idf::Result<Stats> {
        self.presenter.acquire()?;
        scene.advance();
        self.presenter.invalidate(scene.damage());
        scene.build(self.assets[usize::from(scene.width > scene.height)], &mut self.list);
        self.presenter.render(&self.list, &mut self.cache)
    }

    pub fn run(&mut self, p: &Params) -> String {
        match self.run_frames(p) {
            Ok(line) => line,
            Err(err) => format!("failed: {err}"),
        }
    }

    fn start_load(&self) -> idf::Result<()> {
        self.load_stop.store(false, Ordering::Release);
        self.load_bytes.store(0, Ordering::Relaxed);
        let (stop, bytes) = (self.load_stop.clone(), self.load_bytes.clone());
        thread::Builder::new().name("load").stack_size(4096).priority(9).core(Some(1)).spawn(move || {
            let mut from: Vec<u8, Caps> = Vec::with_capacity_in(LOAD_BYTES, Caps::SPIRAM);
            from.resize(LOAD_BYTES, 0x5a);
            let mut to: Vec<u8, Caps> = Vec::with_capacity_in(LOAD_BYTES, Caps::SPIRAM);
            to.resize(LOAD_BYTES, 0);
            while !stop.load(Ordering::Acquire) {
                to.copy_from_slice(&from);
                bytes.fetch_add(LOAD_BYTES as u32, Ordering::Relaxed);
            }
        })?;
        Ok(())
    }

    fn run_frames(&mut self, p: &Params) -> idf::Result<String> {
        let mut scene = self.setup(p)?;
        if p.load {
            self.start_load()?;
        }
        let started = Instant::now();
        for _ in 0..WARMUP_FRAMES {
            self.frame(&mut scene)?;
        }
        let mut periods = Vec::with_capacity(p.frames);
        let mut sum = Stats::default();
        let mut last = Instant::now();
        for _ in 0..p.frames {
            let stats = self.frame(&mut scene)?;
            let now = Instant::now();
            periods.push(now.duration_since(last).as_micros() as u32);
            last = now;
            sum.prepare_us += stats.prepare_us;
            sum.draw_us += stats.draw_us;
            sum.flip_us += stats.flip_us;
            sum.raster_us += stats.raster_us;
            sum.stall_us += stats.stall_us;
            sum.strips += stats.strips;
            sum.glyphs_decoded += stats.glyphs_decoded;
            sum.glyphs_missing += stats.glyphs_missing;
        }
        self.load_stop.store(true, Ordering::Release);
        let load = self.load_bytes.load(Ordering::Relaxed) as f32 / started.elapsed().as_micros() as f32;
        periods.sort_unstable();
        let total: u64 = periods.iter().map(|&v| u64::from(v)).sum();
        let n = p.frames as f32;
        let ms = |v: u32| v as f32 / n / 1000.0;
        Ok(format!(
            "fps={:.1} frame={:.2} p95={:.2} prepare={:.2} draw={:.2} flip={:.2} raster={:.2} stall={:.2} strips={} decoded={} missing={} load={load:.1}MB/s",
            n * 1_000_000.0 / total as f32,
            total as f32 / n / 1000.0,
            periods[periods.len() * 95 / 100] as f32 / 1000.0,
            ms(sum.prepare_us),
            ms(sum.draw_us),
            ms(sum.flip_us),
            ms(sum.raster_us),
            ms(sum.stall_us),
            sum.strips / p.frames as u32,
            sum.glyphs_decoded,
            sum.glyphs_missing,
        ))
    }

    pub fn latency(&mut self, p: &Params) -> String {
        match self.run_latency(p) {
            Ok(line) => line,
            Err(err) => format!("failed: {err}"),
        }
    }

    /// From a state change at a random point of the frame until the frame is
    /// written (`InPlace`) or on the panel (`Swap`).
    fn run_latency(&mut self, p: &Params) -> idf::Result<String> {
        let mut scene = self.setup(p)?;
        for _ in 0..WARMUP_FRAMES {
            self.frame(&mut scene)?;
        }
        self.presenter.wait_visible()?;
        let mut samples = Vec::with_capacity(p.frames);
        let mut seed = 0x2545_F491u32;
        for _ in 0..p.frames {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let delay = Duration::from_micros(u64::from(seed % MAX_FRAME_PERIOD_US));
            let start = Instant::now();
            while start.elapsed() < delay {
                core::hint::spin_loop();
            }
            let t0 = Instant::now();
            self.frame(&mut scene)?;
            self.presenter.wait_visible()?;
            samples.push(t0.elapsed().as_micros() as u32);
        }
        samples.sort_unstable();
        let total: u64 = samples.iter().map(|&v| u64::from(v)).sum();
        let at = |q: usize| samples[(samples.len() - 1) * q / 100] as f32 / 1000.0;
        Ok(format!(
            "avg={:.2} p50={:.2} p95={:.2} max={:.2}",
            total as f32 / samples.len() as f32 / 1000.0,
            at(50),
            at(95),
            at(100),
        ))
    }
}
