/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::CStr;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::slice;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::time::Duration;

use idf::fs::File;
use idf::heap::{self, Caps};
use idf::log::{error, warn};
use idf::sync::{Mutex, MutexGuard, Semaphore};
use idf::thread::{self, JoinHandle};

use crate::error::{Error, Result};

const TAG: &CStr = c"media_buffer";

pub const ARENA_ALIGNMENT: usize = 64;
pub const READ_ALIGNMENT: usize = 64;

pub(crate) const CHUNK_BYTES: usize = 64 * 1024;
const CHUNK: i64 = CHUNK_BYTES as i64;
const DEPTH_CHUNKS: usize = 16;
const WAIT: Duration = Duration::from_millis(20);
const WINDOW_MIN_BYTES: usize = 8 * 1024;
const WINDOW_BYTES: usize = 64 * 1024;
const TASK_STACK_BYTES: usize = 3072;
const TASK_PRIORITY: u32 = 3;
const TASK_CORE: u32 = 0;

fn align_down(value: i64) -> i64 {
    value & !(CHUNK - 1)
}

fn chunk_offset(value: i64) -> usize {
    (value & (CHUNK - 1)) as usize
}

fn chunk_index(value: i64) -> usize {
    (value >> 16) as usize
}

fn chunks_up(value: i64) -> usize {
    ((value + CHUNK - 1) >> 16) as usize
}

const fn bounce_bytes_for(arena_bytes: usize) -> usize {
    arena_bytes / 4 / CHUNK_BYTES * CHUNK_BYTES
}

pub(crate) fn psram_first<T>(alloc: impl Fn(Caps) -> idf::Result<T>) -> Result<T> {
    alloc(Caps::SPIRAM).or_else(|_| alloc(Caps::DEFAULT)).map_err(|_| Error::Memory)
}

trait Storage: Send {
    fn bytes(&mut self) -> &mut [u8];
}

impl<A: DerefMut<Target = [u8]> + Send> Storage for A {
    fn bytes(&mut self) -> &mut [u8] {
        self
    }
}

/// Memory the buffer reads into: a quarter of it, in whole 64 KB chunks, is the
/// bounce area and so the largest view, and the rest is the ring. Aligned to
/// `ARENA_ALIGNMENT`.
pub struct Arena {
    storage: Box<dyn Storage>,
    bytes: usize,
    direct: bool,
}

impl Arena {
    pub fn new(storage: impl DerefMut<Target = [u8]> + Send + 'static) -> Self {
        Self::with(storage, false)
    }

    /// For header and tag reads: no read-ahead, reads go through a window in
    /// the bounce area and views fail.
    pub fn direct(storage: impl DerefMut<Target = [u8]> + Send + 'static) -> Self {
        Self::with(storage, true)
    }

    fn with(mut storage: impl DerefMut<Target = [u8]> + Send + 'static, direct: bool) -> Self {
        let bytes = storage.bytes().len();
        Self { storage: Box::new(storage), bytes, direct }
    }

    pub fn max_view(&self) -> usize {
        bounce_bytes_for(self.bytes)
    }
}

struct State {
    cursor: i64,
    base: i64,
    head: usize,
    filled: usize,
    want: usize,
    readahead: bool,
    win_base: i64,
    win_len: usize,
    win_size: usize,
    interrupted: bool,
    io_error: bool,
    pins: Box<[u16], Caps>,
    bounce_pins: u32,
}

impl State {
    fn restart_window(&mut self, ring_chunks: usize) {
        self.head = (self.head + self.filled) % ring_chunks;
        self.filled = 0;
        self.base = align_down(self.cursor);
    }

    fn drop_consumed(&mut self, ring_chunks: usize) {
        if self.cursor < self.base {
            self.restart_window(ring_chunks);
            return;
        }
        while self.filled > 0 && self.base + CHUNK <= self.cursor {
            self.base += CHUNK;
            self.head = (self.head + 1) % ring_chunks;
            self.filled -= 1;
        }
        if self.filled == 0 {
            self.base = align_down(self.cursor);
        }
    }

    fn filled_end(&self) -> i64 {
        self.base + self.filled as i64 * CHUNK
    }
}

// `tell` runs several times per packet, and on a 32-bit target there is no
// 64-bit atomic. Only the task reading the file moves the cursor, so it always
// sees its own writes whole.
#[derive(Default)]
struct Published {
    low: AtomicU32,
    high: AtomicU32,
}

impl Published {
    fn store(&self, value: i64) {
        self.low.store(value as u32, Ordering::Relaxed);
        self.high.store((value >> 32) as u32, Ordering::Relaxed);
    }

    fn load(&self) -> i64 {
        i64::from(self.high.load(Ordering::Relaxed)) << 32 | i64::from(self.low.load(Ordering::Relaxed))
    }
}

struct Shared {
    file: File,
    size: i64,
    direct: bool,
    ring: *mut u8,
    ring_chunks: usize,
    depth: usize,
    bounce: *mut u8,
    bounce_bytes: usize,
    arena_bytes: usize,
    cursor: Published,
    state: Mutex<State>,
    io: Mutex<()>,
    wake: Semaphore,
    data: Semaphore,
    stop: AtomicBool,
    _storage: Box<dyn Storage>,
}

// The ring and the bounce area are reached through raw pointers. A chunk is
// only written while it is outside the filled window and unpinned, and the
// bounce area only while `bounce_pins` holds it for the writer.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl Shared {
    fn chunk(&self, slot: usize) -> *mut u8 {
        unsafe { self.ring.add(slot * CHUNK_BYTES) }
    }

    fn read_at(&self, offset: i64, out: &mut [u8]) -> usize {
        let _io = self.io.lock();
        let mut got = 0;
        if self.file.seek(offset as u64).is_ok() {
            while got < out.len() {
                match self.file.read(&mut out[got..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got += n,
                }
            }
        }
        got
    }

    fn wait_for<'a>(&'a self, mut s: MutexGuard<'a, State>, end: i64) -> (MutexGuard<'a, State>, bool) {
        loop {
            s.drop_consumed(self.ring_chunks);
            if end <= s.filled_end() {
                return (s, true);
            }
            if !s.readahead || s.io_error || s.interrupted {
                return (s, false);
            }
            let need = chunks_up(end - s.base);
            s.want = s.want.max(need);
            drop(s);
            self.wake.give();
            self.data.take_timeout(WAIT);
            s = self.state.lock();
        }
    }

    fn failure(&self, s: &State) -> Error {
        if s.interrupted { Error::Interrupted } else { Error::Io }
    }

    fn release(&self, pin: Pin) {
        let mut s = self.state.lock();
        match pin {
            Pin::Bounce => s.bounce_pins -= 1,
            Pin::Chunks { slot, count } => {
                for p in &mut s.pins[usize::from(slot)..usize::from(slot) + usize::from(count)] {
                    *p -= 1;
                }
            }
        }
        drop(s);
        self.wake.give();
        self.data.give();
    }

    fn interrupt(&self, interrupted: bool) {
        self.state.lock().interrupted = interrupted;
        self.data.give();
    }

    // Tag parsing reads a 10 byte frame header, then a frame body, then the
    // next header: a fixed 64 KB window makes the first of those cost a 64 KB
    // read. The window starts small and doubles while refills stay sequential.
    // Refills are aligned down to READ_ALIGNMENT to keep the bounce area in
    // phase with the file offset (see `read_alloc`).
    fn read_windowed(&self, s: &mut State, out: &mut [u8]) -> usize {
        let cap = self.bounce_bytes.min(WINDOW_BYTES);
        if s.win_size == 0 {
            s.win_size = WINDOW_MIN_BYTES;
        }
        let size = out.len();
        let mut done = 0;
        while done < size {
            let cursor = s.cursor;
            let mut window = s.win_size.min(cap);
            if size - done >= window {
                let got = self.read_at(cursor, &mut out[done..]);
                s.cursor += got as i64;
                return done + got;
            }
            if s.win_len == 0 || cursor < s.win_base || cursor >= s.win_base + s.win_len as i64 {
                let sequential = s.win_len != 0 && cursor == s.win_base + s.win_len as i64;
                s.win_size = if sequential && window < cap { window * 2 } else { WINDOW_MIN_BYTES };
                window = s.win_size.min(cap);
                s.win_base = cursor & !(READ_ALIGNMENT as i64 - 1);
                let bounce = unsafe { slice::from_raw_parts_mut(self.bounce, window) };
                s.win_len = self.read_at(s.win_base, bounce);
                if s.win_len <= (cursor - s.win_base) as usize {
                    break;
                }
            }
            let offset = (cursor - s.win_base) as usize;
            let take = (s.win_len - offset).min(size - done);
            unsafe { ptr::copy_nonoverlapping(self.bounce.add(offset), out[done..].as_mut_ptr(), take) };
            s.cursor += take as i64;
            done += take;
        }
        done
    }

    fn read(&self, out: &mut [u8]) -> usize {
        let mut s = self.state.lock();
        if s.cursor >= self.size {
            return 0;
        }
        let size = out.len().min((self.size - s.cursor) as usize);
        let out = &mut out[..size];
        if self.direct {
            let got = self.read_windowed(&mut s, out);
            self.cursor.store(s.cursor);
            return got;
        }
        if !s.readahead {
            let offset = s.cursor;
            drop(s);
            let got = self.read_at(offset, out);
            let mut s = self.state.lock();
            s.cursor += got as i64;
            self.cursor.store(s.cursor);
            return got;
        }

        let mut done = 0;
        while done < size {
            let end = (align_down(s.cursor) + CHUNK).min(s.cursor + (size - done) as i64);
            let (next, ok) = self.wait_for(s, end);
            s = next;
            if !ok {
                break;
            }
            let index = chunk_index(s.cursor - s.base);
            let offset = chunk_offset(s.cursor - s.base);
            let take = (end - s.cursor) as usize;
            let chunk = self.chunk((s.head + index) % self.ring_chunks);
            unsafe { ptr::copy_nonoverlapping(chunk.add(offset), out[done..].as_mut_ptr(), take) };
            s.cursor += take as i64;
            done += take;
        }
        self.cursor.store(s.cursor);
        s.want = 0;
        drop(s);
        self.wake.give();
        done
    }

    fn seek(&self, offset: i64) -> i64 {
        let offset = offset.clamp(0, self.size);
        let mut s = self.state.lock();
        s.cursor = offset;
        self.cursor.store(offset);
        s.io_error = false;
        s.drop_consumed(self.ring_chunks);
        drop(s);
        self.wake.give();
        offset
    }

    fn fill_next(&self) -> bool {
        let mut s = self.state.lock();
        let mut target = None;
        if s.readahead && !s.io_error {
            s.drop_consumed(self.ring_chunks);
            let depth = s.want.max(self.depth);
            let offset = s.filled_end();
            if s.filled < depth && offset < self.size {
                let next = (s.head + s.filled) % self.ring_chunks;
                if s.pins[next] == 0 {
                    target = Some((offset, next));
                }
            }
        }
        drop(s);
        let Some((offset, slot)) = target else {
            return false;
        };

        let want = CHUNK_BYTES.min((self.size - offset) as usize);
        let out = unsafe { slice::from_raw_parts_mut(self.chunk(slot), want) };
        let got = self.read_at(offset, out);

        let mut s = self.state.lock();
        if s.readahead && s.filled_end() == offset && (s.head + s.filled) % self.ring_chunks == slot {
            if got == want {
                s.filled += 1;
            } else {
                warn!(TAG, "short read at {offset}: {got} of {want}");
                s.io_error = true;
            }
        }
        drop(s);
        self.data.give();
        true
    }
}

fn view_locked<'a>(this: &'a Arc<Shared, Caps>, mut s: MutexGuard<'a, State>, start: i64, size: usize, advance: bool) -> Result<View> {
    let end = start + size as i64;
    let (mut s, slot, count, offset) = loop {
        if !s.readahead {
            s.want = 0;
            return Err(Error::Io);
        }
        let (mut locked, ok) = this.wait_for(s, end);
        if !ok || start < locked.base {
            locked.want = 0;
            return Err(this.failure(&locked));
        }
        let first = chunk_index(start - locked.base);
        let offset = chunk_offset(start - locked.base);
        let last = chunk_index(end - 1 - locked.base);
        let slot = (locked.head + first) % this.ring_chunks;
        let count = last - first + 1;

        if slot + count <= this.ring_chunks {
            for pin in &mut locked.pins[slot..slot + count] {
                *pin += 1;
            }
            if advance {
                locked.cursor = end;
            }
            locked.want = 0;
            drop(locked);
            this.wake.give();
            let data = unsafe { this.chunk(slot).add(offset) };
            return Ok(View::pinned(this.clone(), data, size, Pin::Chunks { slot: slot as u16, count: count as u16 }));
        }

        if locked.bounce_pins == 0 {
            break (locked, slot, count, offset);
        }
        if locked.interrupted {
            locked.want = 0;
            return Err(Error::Interrupted);
        }
        drop(locked);
        this.data.take_timeout(WAIT);
        s = this.state.lock();
    };
    s.bounce_pins = 1;
    let (base, head) = (s.base, s.head);
    // Pinned for the copy, which runs unlocked: a seek from another task
    // could otherwise hand these chunks back to read-ahead under it.
    for i in 0..count {
        s.pins[(slot + i) % this.ring_chunks] += 1;
    }
    drop(s);

    debug_assert_eq!(offset, chunk_offset(start - base));
    let mut done = 0;
    while done < size {
        let position = start + done as i64;
        let index = chunk_index(position - base);
        let from = chunk_offset(position - base);
        let take = (CHUNK_BYTES - from).min(size - done);
        let chunk = this.chunk((head + index) % this.ring_chunks);
        unsafe { ptr::copy_nonoverlapping(chunk.add(from), this.bounce.add(done), take) };
        done += take;
    }

    let mut s = this.state.lock();
    for i in 0..count {
        s.pins[(slot + i) % this.ring_chunks] -= 1;
    }
    if advance {
        s.cursor = end;
        this.cursor.store(end);
    }
    s.want = 0;
    drop(s);
    this.wake.give();
    Ok(View::pinned(this.clone(), this.bounce, size, Pin::Bounce))
}

fn readahead_task(shared: Arc<Shared, Caps>) {
    while !shared.stop.load(Ordering::Relaxed) {
        if !shared.fill_next() {
            shared.wake.take_timeout(WAIT);
        }
    }
}

/// Wakes a task out of a read or a view. While set, every read and view that
/// has to wait fails; interrupt before waiting for a task that may be inside
/// one and clear it as soon as that task has stopped.
#[derive(Clone)]
pub struct Interrupter(Arc<Shared, Caps>);

impl Interrupter {
    pub fn interrupt(&self, interrupted: bool) {
        self.0.interrupt(interrupted);
    }
}

pub struct Buffer {
    shared: Arc<Shared, Caps>,
    task: Option<JoinHandle<()>>,
}

impl Buffer {
    pub fn open(path: &CStr, mut arena: Arena) -> Result<Self> {
        let (data, arena_bytes) = {
            let bytes = arena.storage.bytes();
            (bytes.as_mut_ptr(), bytes.len())
        };
        let bounce_bytes = bounce_bytes_for(arena_bytes);
        let bounce_chunks = bounce_bytes / CHUNK_BYTES;
        let ring_chunks = (arena_bytes - bounce_bytes) / CHUNK_BYTES;
        let depth = (ring_chunks / 2).min(DEPTH_CHUNKS);
        let window = (bounce_chunks + 1).max(depth);
        if data as usize % ARENA_ALIGNMENT != 0 || bounce_chunks == 0 || ring_chunks < window * 2 {
            error!(TAG, "unusable arena");
            return Err(Error::Arena);
        }

        let file = File::open(path).map_err(|e| {
            error!(TAG, "open {}: {e}", path.to_str().unwrap_or("?"));
            Error::Open
        })?;
        let size = file.size().map_err(|_| Error::Open)? as i64;

        let pins = psram_first(|caps| heap::zeroed_slice(ring_chunks, caps))?;
        let state = State {
            cursor: 0,
            base: 0,
            head: 0,
            filled: 0,
            want: 0,
            readahead: false,
            win_base: 0,
            win_len: 0,
            win_size: 0,
            interrupted: false,
            io_error: false,
            pins,
            bounce_pins: 0,
        };
        let shared = Shared {
            file,
            size,
            direct: arena.direct,
            ring: data,
            ring_chunks,
            depth,
            bounce: unsafe { data.add(ring_chunks * CHUNK_BYTES) },
            bounce_bytes,
            arena_bytes,
            cursor: Published::default(),
            state: Mutex::try_new(state).map_err(|_| Error::Memory)?,
            io: Mutex::try_new(()).map_err(|_| Error::Memory)?,
            wake: Semaphore::try_new(0, 1).map_err(|_| Error::Memory)?,
            data: Semaphore::try_new(0, 1).map_err(|_| Error::Memory)?,
            stop: AtomicBool::new(false),
            _storage: arena.storage,
        };
        let shared = Arc::try_new_in(shared, Caps::DEFAULT).map_err(|_| Error::Memory)?;

        let worker = shared.clone();
        let task = thread::Builder::new()
            .name("media_readahead")
            .stack_size(TASK_STACK_BYTES)
            .priority(TASK_PRIORITY)
            .core(Some(TASK_CORE))
            .stack_caps(Caps::SPIRAM)
            .spawn(move || readahead_task(worker))
            .map_err(|_| {
                error!(TAG, "no memory for the read-ahead task");
                Error::Memory
            })?;
        Ok(Self { shared, task: Some(task) })
    }

    pub fn size(&self) -> i64 {
        self.shared.size
    }

    pub fn tell(&self) -> i64 {
        self.shared.cursor.load()
    }

    pub fn max_view(&self) -> usize {
        self.shared.bounce_bytes
    }

    pub fn arena_bytes(&self) -> usize {
        self.shared.arena_bytes
    }

    /// Reads up to `out.len()` bytes; less at the end of the file, or when
    /// read-ahead failed or was interrupted.
    pub fn read(&self, out: &mut [u8]) -> usize {
        self.shared.read(out)
    }

    pub(crate) fn read_all(&self, out: &mut [u8]) -> bool {
        self.read(out) == out.len()
    }

    pub(crate) fn read_exact_at(&self, at: i64, out: &mut [u8]) -> bool {
        self.seek(at);
        self.read_all(out)
    }

    /// Reads `size` bytes at `offset` into a new allocation. FATFS copies the
    /// leading partial sector through its own window and DMAs the rest straight
    /// to the caller, so the transfer only runs at full speed when the
    /// destination has the phase of the file offset; the allocation is laid
    /// out for that.
    pub fn read_alloc(&self, offset: i64, size: usize) -> Result<Aligned> {
        let skew = (offset as usize) & (READ_ALIGNMENT - 1);
        let mut block = Aligned::new(skew, size)?;
        self.seek(offset);
        if self.read(&mut block) != size {
            return Err(Error::Io);
        }
        Ok(block)
    }

    pub fn seek(&self, offset: i64) -> i64 {
        self.shared.seek(offset)
    }

    pub fn skip(&self, delta: i64) {
        self.seek(self.tell().wrapping_add(delta));
    }

    pub fn set_readahead(&self, enabled: bool) {
        if self.shared.direct {
            return;
        }
        let mut s = self.shared.state.lock();
        s.readahead = enabled;
        if !enabled {
            s.restart_window(self.shared.ring_chunks);
        }
        drop(s);
        self.shared.wake.give();
    }

    /// `size` bytes at the cursor, which moves past them. A view pins the
    /// chunks it covers, and read-ahead waits on a pinned chunk while a view
    /// waits for read-ahead: nothing may hold a view longer than the reader
    /// takes to lap the ring.
    pub fn view(&self, size: usize) -> Result<View> {
        let s = self.shared.state.lock();
        if size == 0 || size > self.shared.bounce_bytes || s.cursor + size as i64 > self.shared.size {
            return Err(Error::Io);
        }
        let start = s.cursor;
        view_locked(&self.shared, s, start, size, true)
    }

    /// `size` bytes at `offset`, leaving the cursor alone unless the view would
    /// end more than half the ring past it, which drops the window.
    pub fn view_at(&self, offset: i64, size: usize) -> Result<View> {
        if offset < 0 || size == 0 || size > self.shared.bounce_bytes || offset + size as i64 > self.shared.size {
            return Err(Error::Io);
        }
        let mut s = self.shared.state.lock();
        let span = offset + size as i64 - align_down(s.cursor);
        if offset < s.cursor || span > (self.shared.ring_chunks / 2) as i64 * CHUNK {
            s.cursor = offset;
            self.shared.cursor.store(offset);
            s.io_error = false;
            s.drop_consumed(self.shared.ring_chunks);
        }
        view_locked(&self.shared, s, offset, size, false)
    }

    pub fn interrupter(&self) -> Interrupter {
        Interrupter(self.shared.clone())
    }

    /// Why a read came up short: `Ok` when it just hit the end.
    pub(crate) fn check(&self) -> Result<()> {
        let s = self.shared.state.lock();
        if s.interrupted {
            Err(Error::Interrupted)
        } else if s.io_error {
            Err(Error::Io)
        } else {
            Ok(())
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.give();
        if let Some(task) = self.task.take() {
            task.join();
        }
    }
}

#[derive(Clone, Copy)]
enum Pin {
    Chunks { slot: u16, count: u16 },
    Bounce,
}

enum Owner {
    Pinned(Arc<Shared, Caps>, Pin),
    Heap(#[allow(dead_code)] Box<[u8], Caps>),
}

/// Packet bytes: a pinned part of the arena, the bounce area, or a copy of
/// their own. Dropping it releases them. It keeps the arena alive, so it may
/// outlive the demuxer.
pub struct View {
    data: *const u8,
    len: usize,
    owner: Owner,
    _bytes: PhantomData<[u8]>,
}

unsafe impl Send for View {}
unsafe impl Sync for View {}

impl View {
    fn pinned(shared: Arc<Shared, Caps>, data: *const u8, len: usize, pin: Pin) -> Self {
        Self { data, len, owner: Owner::Pinned(shared, pin), _bytes: PhantomData }
    }

    pub(crate) fn owned(bytes: Box<[u8], Caps>) -> Self {
        Self { data: bytes.as_ptr(), len: bytes.len(), owner: Owner::Heap(bytes), _bytes: PhantomData }
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.len = self.len.min(len);
    }
}

impl Deref for View {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.data, self.len) }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        if let Owner::Pinned(shared, pin) = &self.owner {
            shared.release(*pin);
        }
    }
}

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Line([u8; READ_ALIGNMENT]);

unsafe impl devkit::Zeroable for Line {}

pub struct Aligned {
    lines: Box<[Line], Caps>,
    start: usize,
    len: usize,
}

impl Aligned {
    pub(crate) fn new(skew: usize, len: usize) -> Result<Self> {
        let lines = (skew + len).div_ceil(READ_ALIGNMENT).max(1);
        let lines = psram_first(|caps| heap::zeroed_slice(lines, caps))?;
        Ok(Self { lines, start: skew, len })
    }

    pub(crate) fn narrow(&mut self, from: usize, len: usize) {
        self.start += from;
        self.len = len;
    }

    pub(crate) fn shrink(&mut self, len: usize) {
        self.len = self.len.min(len);
    }
}

impl Deref for Aligned {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        let bytes = unsafe { slice::from_raw_parts(self.lines.as_ptr().cast::<u8>(), self.lines.len() * READ_ALIGNMENT) };
        &bytes[self.start..self.start + self.len]
    }
}

impl DerefMut for Aligned {
    fn deref_mut(&mut self) -> &mut [u8] {
        let bytes = unsafe { slice::from_raw_parts_mut(self.lines.as_mut_ptr().cast::<u8>(), self.lines.len() * READ_ALIGNMENT) };
        &mut bytes[self.start..self.start + self.len]
    }
}
