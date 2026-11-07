/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Allocator;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering::Relaxed};

use devkit_gfx::simd::{blend565, mix565};
use devkit_gfx::{Argb8888, Bgr888, GlyphCache, ImageFormat, Pixel, Raster, RasterTarget, Rect, Rgb565};
use idf::heap::Caps;

use crate::motion::{Clock, Motion, MotionKind, ease_out};
use crate::node::{NONE, NodeContent, NodeIndex};
use crate::paint::Effects;
use crate::zoom::{CHUNK, Line, gather};
use crate::tree::Tree;

const DEFAULT_MS: u16 = 200;

/// The side of the screen a `Presence::from_edge` is past.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// How an element looks while absent: `.appear` starts from it and
/// `.disappear` ends at it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Presence {
    opacity: u8,
    scale: f32,
    offset: (i16, i16),
    edge: Option<Edge>,
    ms: u16,
}

impl Presence {
    /// Comes or goes at once.
    pub const fn none() -> Self {
        Self { opacity: 255, scale: 1.0, offset: (0, 0), edge: None, ms: 0 }
    }

    pub const fn fade() -> Self {
        Self { opacity: 0, ms: DEFAULT_MS, ..Self::none() }
    }

    /// Just outside the screen past `edge`, wherever the element is placed.
    pub const fn from_edge(edge: Edge) -> Self {
        Self { edge: Some(edge), ms: DEFAULT_MS, ..Self::none() }
    }

    /// Below 1 grows into place, above 1 shrinks into it.
    pub const fn scale(self, scale: f32) -> Self {
        Self { scale, ..self }
    }

    pub const fn offset(self, dx: i16, dy: i16) -> Self {
        Self { offset: (dx, dy), ..self }
    }

    pub const fn ms(self, ms: u16) -> Self {
        Self { ms, ..self }
    }

    pub(crate) fn moves(&self) -> bool {
        self.ms > 0
    }
}

/// Where a node is on its way between absent and shown.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Pose {
    pub(crate) offset: (i16, i16),
    pub(crate) opacity: u8,
    pub(crate) scale: f32,
    /// How far a scaled picture reaches past the node.
    pub(crate) reach: i16,
}

pub(crate) struct Running {
    absent: Presence,
    edge: (i16, i16),
    pub(crate) appearing: bool,
    /// Where the node shows when it is not moved.
    shown: Rect,
    snapshot: Option<Box<Snapshot>>,
    /// A picture to scale is still to be taken.
    pub(crate) capture: bool,
}

impl Running {
    /// 0 absent, 1 shown.
    fn progress(&self, clock: &Clock) -> f32 {
        let t = clock.linear();
        if self.appearing { ease_out(t) } else { ease_out(1.0 - t) }
    }

    fn pose(&self, clock: &Clock, size: (i16, i16)) -> Pose {
        let p = self.progress(clock);
        let q = 1.0 - p;
        let (dx, dy) = (self.absent.offset.0 + self.edge.0, self.absent.offset.1 + self.edge.1);
        let opacity = f32::from(self.absent.opacity) + (255.0 - f32::from(self.absent.opacity)) * p;
        let scale = if self.snapshot.is_some() { self.absent.scale + (1.0 - self.absent.scale) * p } else { 1.0 };
        let reach = if self.absent.scale > 1.0 { ((self.absent.scale - 1.0) * f32::from(size.0.max(size.1)) / 2.0) as i16 + 1 } else { 0 };
        Pose { offset: ((f32::from(dx) * q) as i16, (f32::from(dy) * q) as i16), opacity: opacity as u8, scale, reach }
    }
}

// `core` has no `cbrt`.
fn cbrt(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut y = 1.0;
    for _ in 0..12 {
        y -= (y * y * y - x) / (3.0 * y * y);
    }
    y
}

/// An RGB565 picture of a node at half its size, small enough for internal
/// memory: in PSRAM its bandwidth bounds every frame. Alpha is kept only for
/// rows not opaque all through.
pub(crate) struct Snapshot {
    color: Vec<u16, Caps>,
    alpha: Vec<u8, Caps>,
    /// Where each row's alpha starts; `OPAQUE` for none.
    rows: Vec<u32, Caps>,
    /// The node's size; the picture is half of it, rounded down.
    width: i16,
    height: i16,
    scale: AtomicU32,
    opacity: AtomicU8,
}

const OPAQUE: u32 = u32::MAX;

const SHRINK: usize = 2;

pub(crate) fn half(length: i16) -> usize {
    length as usize / SHRINK
}

fn reserve<T>(count: usize) -> Option<Vec<T, Caps>> {
    for caps in [Caps::INTERNAL, Caps::SPIRAM] {
        let mut memory = Vec::new_in(caps);
        if memory.try_reserve_exact(count).is_ok() {
            return Some(memory);
        }
    }
    None
}

impl Snapshot {
    pub(crate) fn allocate(width: i16, height: i16) -> Option<Self> {
        if half(width) == 0 || half(height) == 0 {
            return None;
        }
        let color: Vec<u16, Caps> = reserve(half(width) * half(height))?;
        let alpha = Vec::new_in(*color.allocator());
        let rows = reserve(half(height))?;
        Some(Self { color, alpha, rows, width, height, scale: AtomicU32::new(1.0f32.to_bits()), opacity: AtomicU8::new(255) })
    }

    /// Adds `rows` rows of half-size ARGB8888 `pixels` below those stored.
    pub(crate) fn store(&mut self, pixels: &[u32], rows: usize) -> Option<()> {
        let columns = half(self.width);
        for line in pixels[..rows * columns].chunks_exact(columns) {
            self.color.extend(line.iter().map(|&argb| ((argb >> 19 & 0x1f) << 11 | (argb >> 10 & 0x3f) << 5 | (argb >> 3 & 0x1f)) as u16));
            if line.iter().all(|&argb| argb >= 0xff00_0000) {
                self.rows.push(OPAQUE);
                continue;
            }
            self.rows.push(self.alpha.len() as u32);
            self.alpha.try_reserve(columns).ok()?;
            self.alpha.extend(line.iter().map(|&argb| (argb >> 24) as u8));
        }
        Some(())
    }

    pub(crate) fn size(&self) -> (i16, i16) {
        (self.width, self.height)
    }

    pub(crate) fn set(&self, scale: f32, opacity: u8) {
        self.scale.store(scale.to_bits(), Relaxed);
        self.opacity.store(opacity, Relaxed);
    }

    fn blend<P: Pixel>(&self, target: &mut RasterTarget<'_>) {
        let scale = f32::from_bits(self.scale.load(Relaxed));
        let opacity = u32::from(self.opacity.load(Relaxed));
        let area = target.area();
        let source = |at: i16, length: i16| {
            let s = ((f32::from(at) + 0.5) / scale) as i32;
            (at >= 0 && s < i32::from(length)).then(|| (s as usize / SHRINK).min(half(length) - 1) as u16)
        };
        // The columns inside the picture are contiguous.
        let columns: Vec<(usize, u16)> = (area.x..area.right()).enumerate().filter_map(|(i, x)| source(x, self.width).map(|s| (i, s))).collect();
        let Some(&(first, _)) = columns.first() else { return };
        let sources: Vec<u16> = columns.iter().map(|c| c.1).collect();
        let width = half(self.width);
        let solid = (255 * opacity).wrapping_add(255) >> 8;
        let offsets: Vec<u16> = sources.iter().map(|&s| s * 2).collect();
        let mut fronts = Line([0; CHUNK]);
        for y in area.y..area.bottom() {
            let Some(sy) = source(y, self.height) else { continue };
            let out = unsafe { target.row(y).as_mut_ptr().add(first * P::BYTES) };
            let color = unsafe { self.color.as_ptr().add(usize::from(sy) * width) };
            let start = self.rows[usize::from(sy)];
            unsafe {
                if start == OPAQUE && P::NATIVE == ImageFormat::Rgb565 {
                    let (out, a5) = (out.cast::<u16>(), solid.wrapping_add(4) >> 3);
                    for (i, chunk) in offsets.chunks(CHUNK).enumerate() {
                        let at = out.add(i * CHUNK);
                        gather(fronts.0.as_mut_ptr(), color, chunk);
                        if a5 >= 32 {
                            core::ptr::copy_nonoverlapping(fronts.0.as_ptr(), at, chunk.len());
                        } else {
                            blend565(at, at, fronts.0.as_ptr(), chunk.len(), a5);
                        }
                    }
                } else if start == OPAQUE {
                    for (i, &sx) in sources.iter().enumerate() {
                        blend_one::<P>(out.add(i * P::BYTES), *color.add(usize::from(sx)), solid);
                    }
                } else {
                    let alpha = self.alpha.as_ptr().add(start as usize);
                    for (i, &sx) in sources.iter().enumerate() {
                        let a = u32::from(*alpha.add(usize::from(sx))).wrapping_mul(opacity).wrapping_add(255) >> 8;
                        if a != 0 {
                            blend_one::<P>(out.add(i * P::BYTES), *color.add(usize::from(sx)), a);
                        }
                    }
                }
            }
        }
    }
}

/// `v` over the pixel at `out` by `a` of 256.
#[inline(always)]
unsafe fn blend_one<P: Pixel>(out: *mut u8, v: u16, a: u32) {
    if P::BYTES == 2 {
        let out = out.cast::<u16>();
        let mixed = if a >= 255 { v } else { mix565(unsafe { out.read_unaligned() }, v, a.wrapping_add(4) >> 3) };
        unsafe { out.write_unaligned(mixed) };
        return;
    }
    let v = u32::from(v);
    let (r, g, b) = (v >> 11, (v >> 5) & 0x3f, v & 0x1f);
    let src = [(b << 3) | (b >> 2), (g << 2) | (g >> 4), (r << 3) | (r >> 2)];
    let keep = 256u32.wrapping_sub(a);
    for (k, &s) in src.iter().enumerate() {
        let d = u32::from(unsafe { out.add(k).read() });
        unsafe { out.add(k).write((s.wrapping_mul(a).wrapping_add(d.wrapping_mul(keep)) >> 8) as u8) };
    }
    if P::BYTES == 4 {
        let d = u32::from(unsafe { out.add(3).read() });
        unsafe { out.add(3).write(a.wrapping_add(d.wrapping_mul(keep) >> 8).min(255) as u8) };
    }
}

impl Raster for Snapshot {
    fn raster(&self, target: &mut RasterTarget<'_>) {
        match target.format() {
            ImageFormat::Argb8888 => self.blend::<Argb8888>(target),
            ImageFormat::Bgr888 => self.blend::<Bgr888>(target),
            _ => self.blend::<Rgb565>(target),
        }
    }
}

impl Tree {
    fn running(&self, id: NodeIndex) -> Option<usize> {
        self.motion(id, |kind| matches!(kind, MotionKind::Presence(_)))
    }

    fn running_of(&self, i: usize) -> Option<(&Running, &Clock)> {
        let motion = &self.motions[i];
        match (&motion.kind, &motion.clock) {
            (MotionKind::Presence(running), Some(clock)) => Some((running, clock)),
            _ => None,
        }
    }

    pub(crate) fn snapshot_of(&self, id: NodeIndex) -> Option<&Snapshot> {
        self.running(id).and_then(|i| self.running_of(i)).and_then(|(r, _)| r.snapshot.as_deref())
    }

    /// Plays `.appear` of the nodes created since the last call, now that they are placed.
    pub(crate) fn start_appearing(&mut self) -> bool {
        let pending = core::mem::take(&mut self.presence_pending);
        for &id in &pending {
            if let Some(presence) = self.nodes[id as usize].appear {
                self.run_presence(id, presence, true);
            }
        }
        !pending.is_empty()
    }

    /// The way a node goes, if it moves.
    fn leaving_presence(&self, id: NodeIndex) -> Option<Presence> {
        let node = &self.nodes[id as usize];
        node.disappear.or(node.appear).filter(Presence::moves)
    }

    /// Keeps a node taken out of the view until its `.disappear` ends.
    pub(crate) fn leave(&mut self, id: NodeIndex) -> bool {
        let Some(presence) = self.leaving_presence(id) else { return false };
        self.nodes[id as usize].leaving = true;
        self.run_presence(id, presence, false);
        true
    }

    /// Plays `.disappear` of every node in `id`; `false` when none moves.
    pub(crate) fn disappear_within(&mut self, id: NodeIndex) -> bool {
        let mut found = Vec::new();
        self.collect_presences(id, &mut found);
        for &(node, presence) in &found {
            self.run_presence(node, presence, false);
        }
        !found.is_empty()
    }

    fn collect_presences(&self, id: NodeIndex, found: &mut Vec<(NodeIndex, Presence)>) {
        if let Some(presence) = self.leaving_presence(id) {
            found.push((id, presence));
        }
        if let NodeContent::Collection(list) = &self.nodes[id as usize].content {
            for &(_, row) in &list.items {
                self.collect_presences(row, found);
            }
        }
        let mut child = self.nodes[id as usize].first_child;
        while child != NONE {
            self.collect_presences(child, found);
            child = self.nodes[child as usize].next;
        }
    }

    /// A node only sliding in or out over `area`: where it shows unmoved, and
    /// how far it is moved from there.
    pub(crate) fn sliding_over(&self, area: Rect) -> Option<(NodeIndex, Rect, (i16, i16))> {
        self.motions.iter().find_map(|m| {
            let MotionKind::Presence(running) = &m.kind else { return None };
            let slides = running.absent.opacity == 255 && running.absent.scale == 1.0 && !running.capture;
            let pose = self.nodes[m.node as usize].pose?;
            (slides && running.shown.intersects(&area)).then_some((m.node, running.shown, pose.offset))
        })
    }

    pub(crate) fn presence_within(&self, id: NodeIndex) -> bool {
        self.motions.iter().filter(|m| matches!(m.kind, MotionKind::Presence(_))).any(|m| {
            let mut node = m.node;
            while node != NONE && node != id {
                node = self.nodes[node as usize].parent;
            }
            node == id
        })
    }

    /// How shown the first node in `id` with a presence is, 1 without one.
    pub(crate) fn presence_level(&self, id: NodeIndex) -> f32 {
        let Some(found) = self.first_presence(id) else { return 1.0 };
        match self.running(found).and_then(|i| self.running_of(i)) {
            Some((running, clock)) => running.progress(clock),
            None if self.nodes[found as usize].pose.is_some() => 0.0,
            None => 1.0,
        }
    }

    fn first_presence(&self, id: NodeIndex) -> Option<NodeIndex> {
        let node = &self.nodes[id as usize];
        if node.appear.is_some() || node.disappear.is_some() {
            return Some(id);
        }
        let mut child = node.first_child;
        while child != NONE {
            if let Some(found) = self.first_presence(child) {
                return Some(found);
            }
            child = self.nodes[child as usize].next;
        }
        None
    }

    fn run_presence(&mut self, id: NodeIndex, absent: Presence, appearing: bool) {
        let shown = match self.running(id) {
            Some(i) => {
                let shown = self.running_of(i).map_or(0.0, |(running, clock)| running.progress(clock));
                self.take_motion(i);
                shown
            }
            None if appearing => 0.0,
            None => 1.0,
        };
        if !absent.moves() {
            self.end_presence(id, appearing);
            return;
        }
        let node = &self.nodes[id as usize];
        let at = node.pose.map_or((0, 0), |p| p.offset);
        let rect = node.screen.offset(-at.0, -at.1);
        let edge = absent.edge.map_or((0, 0), |edge| self.edge_offset(rect, edge));
        let t = if appearing { 1.0 - cbrt(1.0 - shown) } else { cbrt(1.0 - shown) };
        let mut clock = Clock::once(u32::from(absent.ms));
        clock.elapsed = (t * f32::from(absent.ms)) as u32;
        let running = Running { absent, edge, appearing, shown: rect, snapshot: None, capture: absent.scale != 1.0 };
        self.motions.push(Motion::new(id, Some(clock), MotionKind::Presence(running)));
        self.apply_pose(self.motions.len() - 1);
    }

    fn edge_offset(&self, rect: Rect, edge: Edge) -> (i16, i16) {
        match edge {
            Edge::Top => (0, self.area.y - rect.bottom()),
            Edge::Bottom => (0, self.area.bottom() - rect.y),
            Edge::Left => (self.area.x - rect.right(), 0),
            Edge::Right => (self.area.right() - rect.x, 0),
        }
    }

    pub(crate) fn apply_pose(&mut self, i: usize) {
        let id = self.motions[i].node;
        let node = &self.nodes[id as usize];
        let size = (node.rect.w, node.rect.h);
        let Some(pose) = self.running_of(i).map(|(running, clock)| running.pose(clock, size)) else { return };
        let node = &mut self.nodes[id as usize];
        node.pose = Some(pose);
        node.paint_dirty = true;
    }

    /// An appeared node is as its style says; a disappeared one stays absent
    /// until it is removed.
    pub(crate) fn end_presence(&mut self, id: NodeIndex, appearing: bool) {
        let node = &mut self.nodes[id as usize];
        node.paint_dirty = true;
        if appearing {
            node.pose = None;
        } else if node.leaving {
            self.drop_left(id);
        } else {
            node.pose = Some(Pose { offset: (0, 0), opacity: 0, scale: 1.0, reach: 0 });
        }
    }

    fn drop_left(&mut self, id: NodeIndex) {
        let parent = self.nodes[id as usize].parent;
        self.damage_painted(id);
        self.unlink(parent, id);
        self.free_subtree(id);
        self.layout_dirty = true;
    }

    pub(crate) fn capture_pending(&self) -> bool {
        self.motions.iter().any(|m| matches!(&m.kind, MotionKind::Presence(r) if r.capture))
    }

    /// Takes the pictures of what starts to scale this frame. One without
    /// memory for it moves unscaled.
    pub(crate) fn capture_presences<C: Allocator + Clone>(&mut self, effects: &mut Effects, cache: &mut GlyphCache<C>) {
        for i in 0..self.motions.len() {
            let id = self.motions[i].node;
            let MotionKind::Presence(running) = &mut self.motions[i].kind else { continue };
            if !core::mem::take(&mut running.capture) {
                continue;
            }
            let shown = running.shown;
            match self.snapshot(id, shown, effects, cache) {
                Some(snapshot) => {
                    if let MotionKind::Presence(running) = &mut self.motions[i].kind {
                        running.snapshot = Some(Box::new(snapshot));
                    }
                    self.apply_pose(i);
                    let node = &self.nodes[id as usize];
                    let reach = node.pose.map_or(0, |p| p.reach);
                    self.damage.push(node.screen.expand(reach));
                }
                None => idf::log::warn!(c"ui", "no picture to scale a presence, moving it unscaled"),
            }
        }
    }
}
