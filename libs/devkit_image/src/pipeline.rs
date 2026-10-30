/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Two tasks with a bounded ring of rows between them. The default stack is one
//! SIMD kernels can use.

use alloc::sync::Arc;

use idf::heap::Caps;
use idf::sync::{Mutex, Semaphore};
use idf::thread::{self, Builder, JoinHandle};

use crate::mem::{Buffer, buffer, internal_caps};
use crate::{Error, HAVE_PIE, Result};

const DEFAULT_SLOTS: usize = 4;

#[derive(Clone, Copy, Debug)]
pub struct TaskOptions {
    /// `None` leaves the task unpinned.
    pub core: Option<u32>,
    pub priority: u32,
    pub stack_bytes: usize,
    pub stack_caps: Option<Caps>,
}

impl Default for TaskOptions {
    fn default() -> Self {
        let stack_caps = if HAVE_PIE { Some(internal_caps()) } else { None };
        Self { core: None, priority: 5, stack_bytes: 8192, stack_caps }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PipelineOptions {
    pub producer: TaskOptions,
    pub consumer: TaskOptions,
    /// Size of every row through the ring.
    pub row_bytes: usize,
    /// Depth of the ring; 0 means 4.
    pub slots: usize,
    pub caps: Option<Caps>,
}

struct RingState {
    buf: Buffer,
    head: usize,
    tail: usize,
    count: usize,
    closed: bool,
    aborted: bool,
}

/// The bounded ring between the two sides.
pub struct Ring {
    state: Mutex<RingState>,
    // Binary semaphores as edge-triggered wakeups: the condition itself is
    // re-checked under the mutex.
    not_full: Semaphore,
    not_empty: Semaphore,
    row_bytes: usize,
    slots: usize,
}

impl Ring {
    fn new(options: &PipelineOptions) -> Result<Self> {
        if options.row_bytes == 0 {
            return Err(Error::InvalidArgument);
        }
        let slots = if options.slots == 0 { DEFAULT_SLOTS } else { options.slots };
        let buf = buffer(slots * options.row_bytes, options.caps)?;
        let state = RingState { buf, head: 0, tail: 0, count: 0, closed: false, aborted: false };
        Ok(Self {
            state: Mutex::try_new(state)?,
            not_full: Semaphore::try_new(0, 1)?,
            not_empty: Semaphore::try_new(0, 1)?,
            row_bytes: options.row_bytes,
            slots,
        })
    }

    pub const fn row_bytes(&self) -> usize {
        self.row_bytes
    }

    /// Publishes one row, blocking while the ring is full. `InvalidState` once
    /// the pipeline is aborted.
    pub fn put(&self, row: &[u8]) -> Result<()> {
        let row = row.get(..self.row_bytes).ok_or(Error::InvalidArgument)?;
        let mut s = self.state.lock();
        while s.count == self.slots && !s.aborted {
            drop(s);
            self.not_full.take();
            s = self.state.lock();
        }
        if s.aborted {
            return Err(Error::InvalidState);
        }
        let at = s.head * self.row_bytes;
        s.buf[at..at + self.row_bytes].copy_from_slice(row);
        s.head = (s.head + 1) % self.slots;
        s.count += 1;
        drop(s);
        self.not_empty.give();
        Ok(())
    }

    /// Takes one row and returns `true`, blocking while the ring is empty;
    /// `false` at the end of the stream, `InvalidState` once aborted.
    pub fn get(&self, out: &mut [u8]) -> Result<bool> {
        let out = out.get_mut(..self.row_bytes).ok_or(Error::InvalidArgument)?;
        let mut s = self.state.lock();
        while s.count == 0 && !s.closed && !s.aborted {
            drop(s);
            self.not_empty.take();
            s = self.state.lock();
        }
        if s.count == 0 {
            return if s.aborted { Err(Error::InvalidState) } else { Ok(false) };
        }
        let at = s.tail * self.row_bytes;
        out.copy_from_slice(&s.buf[at..at + self.row_bytes]);
        s.tail = (s.tail + 1) % self.slots;
        s.count -= 1;
        drop(s);
        self.not_full.give();
        Ok(true)
    }

    fn close(&self) {
        self.state.lock().closed = true;
        self.not_empty.give();
    }

    /// Makes the other side's pending wait return with the error path.
    pub fn abort(&self) {
        self.state.lock().aborted = true;
        self.not_empty.give();
        self.not_full.give();
    }
}

fn builder(name: &str, options: &TaskOptions) -> Builder {
    let mut b = Builder::new().name(name).stack_size(options.stack_bytes).priority(options.priority).core(options.core);
    if let Some(caps) = options.stack_caps {
        b = b.stack_caps(caps);
    }
    b
}

fn run_producer<P: FnOnce(&Ring) -> Result<()>>(ring: &Ring, producer: P) -> Result<()> {
    let result = producer(ring);
    if result.is_err() {
        ring.abort();
    }
    ring.close();
    result
}

fn run_consumer<C: FnOnce(&Ring) -> Result<()>>(ring: &Ring, consumer: C) -> Result<()> {
    let result = consumer(ring);
    if result.is_err() {
        ring.abort();
    }
    result
}

/// Runs both sides to the end and returns the producer's error, else the
/// consumer's. The closures may borrow from the caller.
pub fn run<P, C>(options: &PipelineOptions, producer: P, consumer: C) -> Result<()>
where
    P: FnOnce(&Ring) -> Result<()> + Send,
    C: FnOnce(&Ring) -> Result<()> + Send,
{
    let ring = Ring::new(options)?;
    let ring = &ring;
    thread::scope(|s| {
        let p = builder("imgf_prod", &options.producer).spawn_scoped(s, move || run_producer(ring, producer))?;
        let c = match builder("imgf_cons", &options.consumer).spawn_scoped(s, move || run_consumer(ring, consumer)) {
            Ok(c) => c,
            Err(e) => {
                ring.abort();
                p.join().ok();
                return Err(e.into());
            }
        };
        let produced = p.join();
        let consumed = c.join();
        produced.and(consumed)
    })
}

/// A pipeline running in the background.
pub struct Job {
    producer: JoinHandle<Result<()>>,
    consumer: JoinHandle<Result<()>>,
}

impl Job {
    /// Both sides have finished.
    pub fn is_finished(&self) -> bool {
        self.producer.is_finished() && self.consumer.is_finished()
    }

    pub fn join(self) -> Result<()> {
        let produced = self.producer.join();
        let consumed = self.consumer.join();
        produced.and(consumed)
    }
}

/// `run` that returns at once, for closures that own what they use.
pub fn start<P, C>(options: &PipelineOptions, producer: P, consumer: C) -> Result<Job>
where
    P: FnOnce(&Ring) -> Result<()> + Send + 'static,
    C: FnOnce(&Ring) -> Result<()> + Send + 'static,
{
    let ring = Arc::new(Ring::new(options)?);
    let p_ring = ring.clone();
    let producer = builder("imgf_prod", &options.producer).spawn(move || run_producer(&p_ring, producer))?;
    let c_ring = ring.clone();
    match builder("imgf_cons", &options.consumer).spawn(move || run_consumer(&c_ring, consumer)) {
        Ok(consumer) => Ok(Job { producer, consumer }),
        Err(e) => {
            ring.abort();
            producer.join().ok();
            Err(e.into())
        }
    }
}
