/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::sync::Arc;
use alloc::vec::Vec;

use idf::heap::{self, Caps};
use idf::sync::{Mutex, Semaphore};

use crate::sample::check_channels;
use crate::{Error, Result, Sample};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Underrun {
    #[default]
    Block,
    Silence,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Overrun {
    #[default]
    Block,
    DropOldest,
}

/// Reads deliver nothing until `prefill` frames are queued, and running dry
/// re-arms the prefill.
#[derive(Clone, Copy, Debug)]
pub struct FifoConfig {
    pub channels: u8,
    /// Frames.
    pub capacity: usize,
    /// Frames, at most `capacity`.
    pub prefill: usize,
    pub underrun: Underrun,
    pub overrun: Overrun,
    pub caps: Caps,
}

impl Default for FifoConfig {
    fn default() -> Self {
        Self {
            channels: 2,
            capacity: 0,
            prefill: 0,
            underrun: Underrun::Block,
            overrun: Overrun::Block,
            caps: Caps::DEFAULT,
        }
    }
}

struct State<S> {
    buf: Vec<S, Caps>,
    head: usize,
    count: usize,
    primed: bool,
    aborted: bool,
    generation: u32,
}

struct Inner<S> {
    state: Mutex<State<S>>,
    readable: Semaphore,
    writable: Semaphore,
    channels: usize,
    capacity: usize,
    prefill: usize,
    underrun: Underrun,
    overrun: Overrun,
}

pub struct Writer<S: Sample>(Arc<Inner<S>, Caps>);
pub struct Reader<S: Sample>(Arc<Inner<S>, Caps>);

/// One writer and one reader, each of which may block. Both return
/// `InvalidState` once aborted.
pub fn new<S: Sample>(config: &FifoConfig) -> Result<(Writer<S>, Reader<S>)> {
    if config.capacity == 0 || config.prefill > config.capacity {
        return Err(Error::InvalidArg);
    }
    check_channels(config.channels)?;
    let channels = usize::from(config.channels);
    let mut buf = heap::try_vec(config.capacity * channels, config.caps)?;
    buf.resize(config.capacity * channels, S::default());
    let state = State { buf, head: 0, count: 0, primed: config.prefill == 0, aborted: false, generation: 0 };
    let inner = Inner {
        state: Mutex::try_new(state)?,
        readable: Semaphore::try_new(0, 1)?,
        writable: Semaphore::try_new(0, 1)?,
        channels,
        capacity: config.capacity,
        prefill: config.prefill,
        underrun: config.underrun,
        overrun: config.overrun,
    };
    let inner = Arc::try_new_in(inner, config.caps).map_err(|_| Error::NoMemory)?;
    Ok((Writer(inner.clone()), Reader(inner)))
}

impl<S: Sample> State<S> {
    fn copy_in(&mut self, src: &[S], frames: usize, capacity: usize, channels: usize) {
        let tail = (self.head + self.count) % capacity;
        let first = frames.min(capacity - tail);
        self.buf[tail * channels..(tail + first) * channels].copy_from_slice(&src[..first * channels]);
        self.buf[..(frames - first) * channels].copy_from_slice(&src[first * channels..frames * channels]);
        self.count += frames;
    }

    fn copy_out(&mut self, dst: &mut [S], frames: usize, capacity: usize, channels: usize) {
        let first = frames.min(capacity - self.head);
        let head = self.head;
        dst[..first * channels].copy_from_slice(&self.buf[head * channels..(head + first) * channels]);
        dst[first * channels..frames * channels].copy_from_slice(&self.buf[..(frames - first) * channels]);
        self.head = (head + frames) % capacity;
        self.count -= frames;
    }
}

impl<S: Sample> Inner<S> {
    fn write(&self, data: &[S]) -> Result<()> {
        let (channels, capacity) = (self.channels, self.capacity);
        let mut src = data;
        let mut frames = data.len() / channels;
        let drop_oldest = self.overrun == Overrun::DropOldest;
        let mut state = self.state.lock();
        if drop_oldest && frames > capacity {
            src = &src[(frames - capacity) * channels..];
            frames = capacity;
        }
        while frames != 0 {
            if state.aborted {
                return Err(Error::InvalidState);
            }
            let mut space = capacity - state.count;
            if drop_oldest && space < frames {
                let dropped = frames - space;
                state.head = (state.head + dropped) % capacity;
                state.count -= dropped;
                space = frames;
            }
            if space == 0 {
                drop(state);
                self.writable.take();
                state = self.state.lock();
                continue;
            }
            let n = frames.min(space);
            state.copy_in(src, n, capacity, channels);
            src = &src[n * channels..];
            frames -= n;
            if !state.primed && state.count >= self.prefill {
                state.primed = true;
            }
            self.readable.give();
        }
        Ok(())
    }

    fn read(&self, data: &mut [S]) -> Result<()> {
        let (channels, capacity) = (self.channels, self.capacity);
        let mut dst = data;
        let mut frames = dst.len() / channels;
        let mut state = self.state.lock();
        while frames != 0 {
            if state.aborted {
                return Err(Error::InvalidState);
            }
            if state.primed {
                let n = frames.min(state.count);
                state.copy_out(dst, n, capacity, channels);
                dst = &mut dst[n * channels..];
                frames -= n;
                if n != 0 {
                    self.writable.give();
                }
                if frames == 0 {
                    break;
                }
                state.primed = self.prefill == 0;
            }
            if self.underrun == Underrun::Silence {
                dst[..frames * channels].fill(S::default());
                break;
            }
            drop(state);
            self.readable.take();
            state = self.state.lock();
        }
        Ok(())
    }

    fn level(&self) -> usize {
        self.state.lock().count
    }

    fn primed(&self) -> bool {
        self.state.lock().primed
    }

    fn generation(&self) -> u32 {
        self.state.lock().generation
    }

    fn flush(&self) {
        let mut state = self.state.lock();
        state.head = 0;
        state.count = 0;
        state.primed = self.prefill == 0;
        state.generation = state.generation.wrapping_add(1);
        self.writable.give();
    }

    fn abort(&self) {
        self.state.lock().aborted = true;
        self.readable.give();
        self.writable.give();
    }
}

impl<S: Sample> Writer<S> {
    pub fn write(&mut self, data: &[S]) -> Result<()> {
        self.0.write(data)
    }

    pub fn level(&self) -> usize {
        self.0.level()
    }

    pub fn primed(&self) -> bool {
        self.0.primed()
    }

    /// Drops everything queued and re-arms the prefill.
    pub fn flush(&self) {
        self.0.flush();
    }

    /// Wakes a blocked writer and reader for teardown.
    pub fn abort(&self) {
        self.0.abort();
    }

    pub fn channels(&self) -> u8 {
        self.0.channels as u8
    }
}

impl<S: Sample> Reader<S> {
    pub fn read(&mut self, data: &mut [S]) -> Result<()> {
        self.0.read(data)
    }

    pub fn level(&self) -> usize {
        self.0.level()
    }

    pub fn primed(&self) -> bool {
        self.0.primed()
    }

    pub fn flush(&self) {
        self.0.flush();
    }

    pub fn abort(&self) {
        self.0.abort();
    }

    pub fn channels(&self) -> u8 {
        self.0.channels as u8
    }

    pub(crate) fn generation(&self) -> u32 {
        self.0.generation()
    }
}
