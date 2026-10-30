/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::CStr;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use core::time::Duration;

use bsp_rs::display::{Display, FrameBuffer, PixelFormat};
use idf::heap::Caps;
use idf::log::error;
use idf::ppa::{ColorMode, Rotation, Srm, SrmOp};
use idf::sync::Semaphore;
use idf::thread;
use idf::time::Instant;

use crate::raster::{Bgr888, Pixel, Rect, Rgb565, Surface};
use crate::scene::{self, Atlas, Kind, Op, Scene};

const TAG: &CStr = c"bench";

const LANES: usize = 2;
const STRIP_BYTES: usize = 64 * 1024;
const STRIP_ROW_ALIGN: i32 = 16;
const MAX_FBS: usize = 3;

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Line([u8; 64]);

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
}

struct Job {
    ops: Vec<Op, Caps>,
    strips: Vec<Rect, Caps>,
    fb: Option<FrameBuffer>,
    rotated: bool,
}

struct Shared {
    start: [Semaphore; LANES],
    done: Semaphore,
    next: AtomicUsize,
    raster_us: [AtomicU32; LANES],
    wait_us: [AtomicU32; LANES],
    job: UnsafeCell<Job>,
    atlas: Atlas,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

struct Lane {
    id: usize,
    srm: Srm,
    buffers: [Vec<Line, Caps>; 2],
    turn: usize,
    last: Rect,
}

impl Lane {
    fn run(mut self, shared: Arc<Shared>) {
        loop {
            shared.start[self.id].take();
            let job = unsafe { &*shared.job.get() };
            let (mut raster, mut wait) = (0, 0);
            match job.fb.map(|fb| fb.format()) {
                Some(PixelFormat::Rgb888) => self.strips::<Bgr888>(&shared, job, &mut raster, &mut wait),
                _ => self.strips::<Rgb565>(&shared, job, &mut raster, &mut wait),
            }
            let t = Instant::now();
            while self.srm.pending() > 0 {
                self.wait_srm();
            }
            wait += t.elapsed().as_micros() as u32;
            shared.raster_us[self.id].store(raster, Ordering::Relaxed);
            shared.wait_us[self.id].store(wait, Ordering::Relaxed);
            shared.done.give();
        }
    }

    fn wait_srm(&mut self) {
        while !self.srm.wait_timeout(Duration::from_secs(1)) {
            error!(TAG, "lane {}: srm stalled, last {:?} pending {}", self.id, self.last, self.srm.pending());
        }
    }

    fn strips<P: Pixel>(&mut self, shared: &Shared, job: &Job, raster: &mut u32, wait: &mut u32) {
        let Some(fb) = job.fb else { return };
        loop {
            let index = shared.next.fetch_add(1, Ordering::Relaxed);
            let Some(strip) = job.strips.get(index) else { return };
            if self.srm.pending() == 2 {
                let t = Instant::now();
                self.wait_srm();
                *wait += t.elapsed().as_micros() as u32;
            }
            let t = Instant::now();
            let buffer = self.buffers[self.turn].as_mut_ptr().cast::<u8>();
            self.turn = (self.turn + 1) % 2;
            self.last = *strip;
            let surface = Surface { ptr: buffer, area: *strip };
            scene::draw::<P>(&job.ops, &shared.atlas, &surface);
            *raster += t.elapsed().as_micros() as u32;
            let result = unsafe { self.srm.start(&srm_op(&fb, strip, buffer, job.rotated)) };
            if let Err(err) = result {
                error!(TAG, "srm {strip:?}: {err}");
            }
        }
    }
}

fn srm_op(fb: &FrameBuffer, strip: &Rect, src: *const u8, rotated: bool) -> SrmOp {
    let size = fb.size();
    let (x, y, rotation) = if rotated {
        (strip.y, i32::from(size.height) - strip.x - strip.w, Rotation::R90)
    } else {
        (strip.x, strip.y, Rotation::R0)
    };
    SrmOp {
        src,
        src_width: strip.w as u32,
        src_height: strip.h as u32,
        dst: fb.as_ptr(),
        dst_size: fb.len() as u32,
        dst_width: u32::from(size.width),
        dst_height: u32::from(size.height),
        dst_x: x as u32,
        dst_y: y as u32,
        format: if fb.format() == PixelFormat::Rgb888 { ColorMode::Rgb888 } else { ColorMode::Rgb565 },
        rotation,
    }
}

pub struct Bench {
    display: &'static Display,
    shared: Arc<Shared>,
}

impl Bench {
    pub fn new(display: &'static Display) -> idf::Result<Self> {
        let shared = Arc::new(Shared {
            start: [Semaphore::try_new(0, 1)?, Semaphore::try_new(0, 1)?],
            done: Semaphore::try_new(0, LANES as u32)?,
            next: AtomicUsize::new(0),
            raster_us: [AtomicU32::new(0), AtomicU32::new(0)],
            wait_us: [AtomicU32::new(0), AtomicU32::new(0)],
            job: UnsafeCell::new(Job {
                ops: Vec::with_capacity_in(2048, Caps::DMA | Caps::INTERNAL),
                strips: Vec::with_capacity_in(256, Caps::DMA | Caps::INTERNAL),
                fb: None,
                rotated: false,
            }),
            atlas: Atlas::new(),
        });
        for id in 0..LANES {
            let buffer = || {
                let mut v = Vec::with_capacity_in(STRIP_BYTES / 64, Caps::DMA | Caps::INTERNAL);
                v.resize(STRIP_BYTES / 64, Line([0; 64]));
                v
            };
            let lane = Lane { id, srm: Srm::new(2)?, buffers: [buffer(), buffer()], turn: 0, last: Rect::default() };
            let shared = shared.clone();
            thread::Builder::new()
                .name("lane")
                .stack_size(8192)
                .priority(10)
                .core(Some(id as u32))
                .spawn(move || lane.run(shared))?;
        }
        Ok(Self { display, shared })
    }

    fn render(&self, fb: FrameBuffer, rotated: bool, area: Rect, lanes: usize) -> u32 {
        let shared = &*self.shared;
        let job = unsafe { &mut *shared.job.get() };
        job.fb = Some(fb);
        job.rotated = rotated;
        job.strips.clear();
        let stride = area.w as usize * fb.format().bytes();
        let rows = (((STRIP_BYTES / stride) as i32) & !(STRIP_ROW_ALIGN - 1)).max(STRIP_ROW_ALIGN);
        let mut y = area.y;
        while y < area.y + area.h {
            let h = rows.min(area.y + area.h - y);
            job.strips.push(Rect::new(area.x, y, area.w, h));
            y += h;
        }
        shared.next.store(0, Ordering::Relaxed);
        // Lane 0 preempts this task on its core, so it must start last.
        for start in shared.start[..lanes].iter().rev() {
            start.give();
        }
        for _ in 0..lanes {
            shared.done.take();
        }
        job.strips.len() as u32
    }

    pub fn run(&self, p: &Params) -> String {
        let display = self.display;
        let size = display.size();
        let (width, height) = (i32::from(size.width), i32::from(size.height));
        let (width, height) = if p.rotated { (height, width) } else { (width, height) };
        let mut scene = Scene { kind: p.kind, width, height, offset: 0, speed: p.speed };
        let fbs = match p.present {
            Present::InPlace => 1,
            Present::Swap2 => 2,
            Present::Swap3 => 3,
        };
        let job = unsafe { &mut *self.shared.job.get() };
        let mut damage = [scene.full(); MAX_FBS];
        let mut latest = display.scanning().unwrap_or(0);
        if p.present == Present::InPlace {
            latest = 0;
            if let Err(err) = display.present(0) {
                return format!("present: {err}");
            }
        }
        let mut frame_us = Vec::with_capacity(p.frames);
        let (mut build, mut render, mut flip, mut raster, mut stall, mut strips) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        let mut last = Instant::now();
        for frame in 0..p.frames + 8 {
            let target = match p.present {
                Present::InPlace => 0,
                Present::Swap2 => {
                    while display.scanning() != Some(latest) {
                        if let Err(err) = display.wait_frame() {
                            return format!("wait_frame: {err}");
                        }
                    }
                    (latest + 1) % fbs
                }
                Present::Swap3 => {
                    let scanning = display.scanning().unwrap_or(0);
                    (0..fbs).find(|&i| i != scanning && i != latest).unwrap_or((latest + 1) % fbs)
                }
            };
            let t0 = Instant::now();
            scene.advance();
            for d in &mut damage {
                *d = d.union(&scene.damage());
            }
            scene.build(&mut job.ops);
            let t1 = Instant::now();
            let Some(fb) = display.frame_buffer(target) else { return format!("no framebuffer {target}") };
            let n = self.render(fb, p.rotated, damage[target], p.lanes);
            damage[target] = Rect::default();
            let t2 = Instant::now();
            if p.present != Present::InPlace {
                while display.scanning() != Some(latest) {
                    if let Err(err) = display.wait_frame() {
                        return format!("wait_frame: {err}");
                    }
                }
                if let Err(err) = display.present(target) {
                    return format!("present: {err}");
                }
                latest = target;
            }
            let t3 = Instant::now();
            if frame >= 8 {
                frame_us.push(t3.duration_since(last).as_micros() as u32);
                build += t1.duration_since(t0).as_micros() as u64;
                render += t2.duration_since(t1).as_micros() as u64;
                flip += t3.duration_since(t2).as_micros() as u64;
                strips += u64::from(n);
                for lane in 0..p.lanes {
                    raster += u64::from(self.shared.raster_us[lane].load(Ordering::Relaxed));
                    stall += u64::from(self.shared.wait_us[lane].load(Ordering::Relaxed));
                }
            }
            last = t3;
        }
        frame_us.sort_unstable();
        let n = p.frames as u64;
        let total: u64 = frame_us.iter().map(|&v| u64::from(v)).sum();
        let ms = |v: u64| v as f32 / n as f32 / 1000.0;
        format!(
            "fps={:.1} frame={:.2} p95={:.2} build={:.2} render={:.2} raster={:.2} ppa_wait={:.2} flip={:.2} strips={}",
            n as f32 * 1_000_000.0 / total as f32,
            ms(total),
            frame_us[frame_us.len() * 95 / 100] as f32 / 1000.0,
            ms(build),
            ms(render),
            ms(raster),
            ms(stall),
            ms(flip),
            strips / n,
        )
    }
}
