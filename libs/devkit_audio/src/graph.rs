/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem;

use idf::heap::{self, Caps};

use crate::convert::convert;
use crate::drift::Drift;
use crate::eq::Eq;
use crate::fifo::{Reader, Writer};
use crate::gain::Gain;
use crate::mixer::Mixer;
use crate::resampler::{Resampler, ResamplerControl};
use crate::sample::check_channels;
use crate::{Error, Format, Pcm, PcmMut, Result, Sample};

const DEFAULT_MAX_FRAMES: usize = 512;

#[derive(Clone, Copy, Debug)]
pub struct GraphConfig {
    /// `write`/`read` chunk; 0 = 512.
    pub max_frames: usize,
    pub caps: Caps,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self { max_frames: 0, caps: Caps::DEFAULT }
    }
}

/// A rate-preserving node; `out` has the node's own format.
pub trait Node: Send {
    fn process(&mut self, inputs: &[Pcm<'_>], out: PcmMut<'_>, frames: usize) -> Result<()>;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NodeId(usize);

const INVALID: NodeId = NodeId(usize::MAX);

trait Block: Send {
    fn process(&mut self, inputs: &[Pcm<'_>], out: PcmMut<'_>, frames: usize) -> Result<()>;
}

fn single<'x, S: Sample>(inputs: &[Pcm<'x>], index: usize, samples: usize) -> Result<&'x [S]> {
    match S::from_pcm(inputs[index]) {
        Some(s) => Ok(&s[..samples]),
        None => Err(Error::InvalidState),
    }
}

impl<S: Sample> Block for Eq<S> {
    fn process(&mut self, inputs: &[Pcm<'_>], out: PcmMut<'_>, _frames: usize) -> Result<()> {
        let out = S::from_pcm_mut(out).ok_or(Error::InvalidState)?;
        Eq::process(self, single(inputs, 0, out.len())?, out);
        Ok(())
    }
}

impl<S: Sample> Block for Gain<S> {
    fn process(&mut self, inputs: &[Pcm<'_>], out: PcmMut<'_>, _frames: usize) -> Result<()> {
        let out = S::from_pcm_mut(out).ok_or(Error::InvalidState)?;
        Gain::process(self, single(inputs, 0, out.len())?, out);
        Ok(())
    }
}

impl<S: Sample> Block for Mixer<S> {
    fn process(&mut self, inputs: &[Pcm<'_>], out: PcmMut<'_>, frames: usize) -> Result<()> {
        let out = S::from_pcm_mut(out).ok_or(Error::InvalidState)?;
        for n in 0..inputs.len() {
            single::<S>(inputs, n, frames * usize::from(self.in_channels(n)))?;
        }
        self.process_with(|n| S::from_pcm(inputs[n]), out);
        Ok(())
    }
}

struct Custom<'a>(Box<dyn Node + 'a>);

impl Block for Custom<'_> {
    fn process(&mut self, inputs: &[Pcm<'_>], out: PcmMut<'_>, frames: usize) -> Result<()> {
        self.0.process(inputs, out, frames)
    }
}

trait Rate: Send {
    fn frames_needed(&mut self, out_frames: usize) -> usize;
    fn process(&mut self, input: Pcm<'_>, out: PcmMut<'_>) -> Result<usize>;
    fn max_out(&self, in_frames: usize) -> usize;
    fn max_in(&self, out_frames: usize) -> usize;
    fn max_in_frames(&self) -> usize;
}

impl<S: Sample> Rate for Resampler<S> {
    fn frames_needed(&mut self, out_frames: usize) -> usize {
        Resampler::frames_needed(self, out_frames)
    }

    fn process(&mut self, input: Pcm<'_>, out: PcmMut<'_>) -> Result<usize> {
        let input = S::from_pcm(input).ok_or(Error::InvalidState)?;
        let out = S::from_pcm_mut(out).ok_or(Error::InvalidState)?;
        Resampler::process(self, input, out)
    }

    fn max_out(&self, in_frames: usize) -> usize {
        Resampler::max_out(self, in_frames)
    }

    fn max_in(&self, out_frames: usize) -> usize {
        Resampler::max_in(self, out_frames)
    }

    fn max_in_frames(&self) -> usize {
        Resampler::max_in_frames(self)
    }
}

trait FifoIn: Send {
    fn write(&mut self, data: Pcm<'_>) -> Result<()>;
}

impl<S: Sample> FifoIn for Writer<S> {
    fn write(&mut self, data: Pcm<'_>) -> Result<()> {
        Writer::write(self, S::from_pcm(data).ok_or(Error::InvalidState)?)
    }
}

trait FifoOut: Send {
    fn read(&mut self, data: PcmMut<'_>) -> Result<()>;
    fn level(&self) -> usize;
    fn primed(&self) -> bool;
    fn generation(&self) -> u32;
}

impl<S: Sample> FifoOut for Reader<S> {
    fn read(&mut self, data: PcmMut<'_>) -> Result<()> {
        Reader::read(self, S::from_pcm_mut(data).ok_or(Error::InvalidState)?)
    }

    fn level(&self) -> usize {
        Reader::level(self)
    }

    fn primed(&self) -> bool {
        Reader::primed(self)
    }

    fn generation(&self) -> u32 {
        Reader::generation(self)
    }
}

type SourceFn<'a> = Box<dyn FnMut(PcmMut<'_>) -> usize + Send + 'a>;
type SinkFn<'a> = Box<dyn FnMut(Pcm<'_>) -> Result<()> + Send + 'a>;

struct DriftState {
    drift: Drift,
    control: ResamplerControl,
    fifo: usize,
    generation: u32,
    elapsed: usize,
}

enum Kind<'a> {
    Input,
    Source(SourceFn<'a>),
    Fifo { input: Option<Box<dyn FifoIn + 'a>>, output: Box<dyn FifoOut + 'a> },
    Block(Box<dyn Block + 'a>),
    Convert,
    Resampler { rate: Box<dyn Rate + 'a>, drift: Option<DriftState> },
    Sink(SinkFn<'a>),
}

#[derive(Default)]
enum Buffer {
    #[default]
    None,
    S16(Vec<i16, Caps>),
    S32(Vec<i32, Caps>),
}

impl Buffer {
    fn alloc(format: Format, len: usize, caps: Caps) -> Result<Self> {
        Ok(match format {
            Format::S16 => {
                let mut v = heap::try_vec(len, caps)?;
                v.resize(len, 0);
                Buffer::S16(v)
            }
            Format::S32 => {
                let mut v = heap::try_vec(len, caps)?;
                v.resize(len, 0);
                Buffer::S32(v)
            }
        })
    }

    fn pcm(&self, samples: usize) -> Pcm<'_> {
        match self {
            Buffer::None => Pcm::S16(&[]),
            Buffer::S16(v) => Pcm::S16(&v[..samples]),
            Buffer::S32(v) => Pcm::S32(&v[..samples]),
        }
    }

    fn pcm_mut(&mut self, samples: usize) -> PcmMut<'_> {
        match self {
            Buffer::None => PcmMut::S16(&mut []),
            Buffer::S16(v) => PcmMut::S16(&mut v[..samples]),
            Buffer::S32(v) => PcmMut::S32(&mut v[..samples]),
        }
    }

    fn zero(&mut self, from: usize, to: usize) {
        match self {
            Buffer::None => {}
            Buffer::S16(v) => v[from..to].fill(0),
            Buffer::S32(v) => v[from..to].fill(0),
        }
    }
}

struct Entry<'a> {
    kind: Kind<'a>,
    format: Format,
    channels: usize,
    srcs: Vec<usize, Caps>,
    consumers: Vec<usize, Caps>,
    push: bool,
    cap: usize,
    buf: Buffer,
    // Filled and cleared around each process call, so it never outlives the
    // buffers it borrows.
    inputs: Vec<Pcm<'static>, Caps>,
}

/// Collects the nodes. `add_*` report a mistake through [`build`](Self::build).
pub struct GraphBuilder<'a> {
    max_frames: usize,
    caps: Caps,
    nodes: Vec<Entry<'a>, Caps>,
    err: Option<Error>,
}

/// A built graph. See the crate's README for the push and pull rules.
pub struct Graph<'a> {
    max_frames: usize,
    nodes: Vec<Entry<'a>, Caps>,
}

impl<'a> GraphBuilder<'a> {
    pub fn new(config: &GraphConfig) -> Self {
        Self {
            max_frames: if config.max_frames == 0 { DEFAULT_MAX_FRAMES } else { config.max_frames },
            caps: config.caps,
            nodes: Vec::new_in(config.caps),
            err: None,
        }
    }

    fn fail(&mut self, err: Error) -> NodeId {
        self.err.get_or_insert(err);
        INVALID
    }

    fn entry(&self, id: NodeId) -> Option<&Entry<'a>> {
        self.nodes.get(id.0)
    }

    fn matches(&self, id: NodeId, format: Format, channels: u8) -> bool {
        self.entry(id).is_some_and(|n| n.format == format && n.channels == usize::from(channels))
    }

    fn add(&mut self, kind: Kind<'a>, srcs: &[NodeId], format: Format, channels: u8) -> NodeId {
        if self.err.is_some() {
            return INVALID;
        }
        if check_channels(channels).is_err() {
            return self.fail(Error::InvalidArg);
        }
        for src in srcs {
            match self.entry(*src) {
                None | Some(Entry { kind: Kind::Sink(_), .. }) => return self.fail(Error::InvalidArg),
                Some(_) => {}
            }
        }
        let entry = (|| {
            let mut ids = heap::try_vec(srcs.len(), self.caps)?;
            ids.extend(srcs.iter().map(|s| s.0));
            self.nodes.try_reserve(1).map_err(|_| idf::Error::NO_MEM)?;
            Ok::<_, idf::Error>(Entry {
                kind,
                format,
                channels: usize::from(channels),
                srcs: ids,
                consumers: Vec::new_in(self.caps),
                push: false,
                cap: 0,
                buf: Buffer::None,
                inputs: heap::try_vec(srcs.len(), self.caps)?,
            })
        })();
        match entry {
            Ok(entry) => {
                self.nodes.push(entry);
                NodeId(self.nodes.len() - 1)
            }
            Err(_) => self.fail(Error::NoMemory),
        }
    }

    fn boxed<T>(&mut self, value: T) -> Option<Box<T>> {
        match Box::try_new(value) {
            Ok(b) => Some(b),
            Err(_) => {
                self.fail(Error::NoMemory);
                None
            }
        }
    }

    /// Starts a push section: [`Graph::write`] runs it on the caller's task.
    pub fn add_input<S: Sample>(&mut self, channels: u8) -> NodeId {
        self.add(Kind::Input, &[], S::FORMAT, channels)
    }

    /// Starts a pull section. `read` returns the frames produced; the rest of
    /// the request plays as silence.
    pub fn add_source<S: Sample>(&mut self, channels: u8, mut read: impl FnMut(&mut [S]) -> usize + Send + 'a) -> NodeId {
        let per = usize::from(channels).max(1);
        let f = move |out: PcmMut<'_>| S::from_pcm_mut(out).map_or(0, &mut read).min(usize::MAX / per);
        let Some(f) = self.boxed(f) else { return INVALID };
        self.add(Kind::Source(f), &[], S::FORMAT, channels)
    }

    /// A FIFO fed with [`Writer::write`] from outside the graph.
    pub fn add_fifo<S: Sample>(&mut self, reader: Reader<S>) -> NodeId {
        let channels = reader.channels();
        let Some(output) = self.boxed(reader) else { return INVALID };
        self.add(Kind::Fifo { input: None, output }, &[], S::FORMAT, channels)
    }

    /// A FIFO fed by `src`: the one node that turns push into pull.
    pub fn add_fifo_from<S: Sample>(&mut self, src: NodeId, writer: Writer<S>, reader: Reader<S>) -> NodeId {
        let channels = reader.channels();
        if !self.matches(src, S::FORMAT, channels) {
            return self.fail(Error::InvalidArg);
        }
        let Some(input) = self.boxed(writer) else { return INVALID };
        let Some(output) = self.boxed(reader) else { return INVALID };
        self.add(Kind::Fifo { input: Some(input), output }, &[src], S::FORMAT, channels)
    }

    pub fn add_eq<S: Sample>(&mut self, src: NodeId, eq: Eq<S>) -> NodeId {
        let channels = eq.channels();
        if !self.matches(src, S::FORMAT, channels) {
            return self.fail(Error::InvalidArg);
        }
        let Some(block) = self.boxed(eq) else { return INVALID };
        self.add(Kind::Block(block), &[src], S::FORMAT, channels)
    }

    pub fn add_gain<S: Sample>(&mut self, src: NodeId, gain: Gain<S>) -> NodeId {
        let channels = gain.channels();
        if !self.matches(src, S::FORMAT, channels) {
            return self.fail(Error::InvalidArg);
        }
        let Some(block) = self.boxed(gain) else { return INVALID };
        self.add(Kind::Block(block), &[src], S::FORMAT, channels)
    }

    pub fn add_convert(&mut self, src: NodeId, format: Format) -> NodeId {
        let Some(channels) = self.entry(src).map(|n| n.channels as u8) else {
            return self.fail(Error::InvalidArg);
        };
        self.add(Kind::Convert, &[src], format, channels)
    }

    /// `srcs` has one node per mixer input.
    pub fn add_mixer<S: Sample>(&mut self, srcs: &[NodeId], mixer: Mixer<S>) -> NodeId {
        if srcs.len() != mixer.num_inputs() {
            return self.fail(Error::InvalidArg);
        }
        for (i, src) in srcs.iter().enumerate() {
            if !self.matches(*src, S::FORMAT, mixer.in_channels(i)) {
                return self.fail(Error::InvalidArg);
            }
        }
        let channels = mixer.out_channels();
        let Some(block) = self.boxed(mixer) else { return INVALID };
        self.add(Kind::Block(block), srcs, S::FORMAT, channels)
    }

    /// With a drift, the graph steers the resampler from the FIFO upstream of it.
    pub fn add_resampler<S: Sample>(
        &mut self,
        src: NodeId,
        resampler: Resampler<S>,
        drift: Option<Drift>,
    ) -> NodeId {
        let channels = resampler.channels();
        if !self.matches(src, S::FORMAT, channels) {
            return self.fail(Error::InvalidArg);
        }
        let drift = match drift {
            None => None,
            Some(drift) => {
                let mut up = Some(src.0);
                let mut fifo = None;
                while let Some(id) = up {
                    let node = &self.nodes[id];
                    if matches!(node.kind, Kind::Fifo { .. }) {
                        fifo = Some(id);
                        break;
                    }
                    up = if node.srcs.len() == 1 { Some(node.srcs[0]) } else { None };
                }
                let Some(fifo) = fifo else { return self.fail(Error::InvalidArg) };
                Some(DriftState { drift, control: resampler.control(), fifo, generation: 0, elapsed: 0 })
            }
        };
        let Some(rate) = self.boxed(resampler) else { return INVALID };
        self.add(Kind::Resampler { rate, drift }, &[src], S::FORMAT, channels)
    }

    pub fn add_node(&mut self, srcs: &[NodeId], format: Format, channels: u8, node: impl Node + 'a) -> NodeId {
        if srcs.is_empty() {
            return self.fail(Error::InvalidArg);
        }
        let Some(node) = self.boxed(node) else { return INVALID };
        let Some(block) = self.boxed(Custom(node)) else { return INVALID };
        self.add(Kind::Block(block), srcs, format, channels)
    }

    /// A sink that blocks paces the writer.
    pub fn add_sink<S: Sample>(&mut self, src: NodeId, mut write: impl FnMut(&[S]) -> Result<()> + Send + 'a) -> NodeId {
        let Some(n) = self.entry(src) else { return self.fail(Error::InvalidArg) };
        let (format, channels) = (n.format, n.channels as u8);
        if format != S::FORMAT {
            return self.fail(Error::InvalidArg);
        }
        let f = move |data: Pcm<'_>| write(S::from_pcm(data).ok_or(Error::InvalidState)?);
        let Some(f) = self.boxed(f) else { return INVALID };
        self.add(Kind::Sink(f), &[src], format, channels)
    }

    fn link_consumers(&mut self) -> Result<()> {
        for id in 0..self.nodes.len() {
            for i in 0..self.nodes[id].srcs.len() {
                let src = self.nodes[id].srcs[i];
                let consumers = &mut self.nodes[src].consumers;
                consumers.try_reserve(1).map_err(|_| Error::NoMemory)?;
                consumers.push(id);
            }
        }
        Ok(())
    }

    fn assign_direction(&mut self, id: usize) -> Result<()> {
        let srcs = &self.nodes[id].srcs;
        let pushed = srcs.iter().filter(|&&s| self.nodes[s].push).count();
        let first_push = srcs.first().is_some_and(|&s| self.nodes[s].push);
        let num_srcs = srcs.len();
        let node = &mut self.nodes[id];
        match node.kind {
            Kind::Input => node.push = true,
            Kind::Source(_) => node.push = false,
            Kind::Fifo { .. } => {
                node.push = false;
                if num_srcs != 0 && !first_push {
                    return Err(Error::InvalidArg);
                }
            }
            Kind::Sink(_) => {
                node.push = true;
                if !first_push {
                    return Err(Error::InvalidArg);
                }
            }
            _ if num_srcs > 1 => {
                node.push = pushed == 1;
                if pushed > 1 {
                    return Err(Error::InvalidArg);
                }
            }
            _ => node.push = first_push,
        }
        Ok(())
    }

    fn push_in_cap(&self, id: usize) -> usize {
        self.nodes[id].srcs.iter().map(|&s| &self.nodes[s]).find(|s| s.push).map_or(0, |s| s.cap)
    }

    fn pull_demand(&self, id: usize) -> usize {
        let Some(&c) = self.nodes[id].consumers.first() else { return self.max_frames };
        let consumer = &self.nodes[c];
        if consumer.push {
            return self.push_in_cap(c);
        }
        match &consumer.kind {
            Kind::Resampler { rate, .. } => rate.max_in(consumer.cap),
            _ => consumer.cap,
        }
    }

    fn size_buffers(&mut self) -> Result<()> {
        for id in 0..self.nodes.len() {
            if !self.nodes[id].push {
                continue;
            }
            let cap = match &self.nodes[id].kind {
                Kind::Input => self.max_frames,
                Kind::Resampler { rate, .. } => rate.max_out(self.push_in_cap(id)),
                _ => self.push_in_cap(id),
            };
            self.nodes[id].cap = cap;
        }
        for id in (0..self.nodes.len()).rev() {
            if !self.nodes[id].push {
                self.nodes[id].cap = self.pull_demand(id);
            }
        }
        for id in 0..self.nodes.len() {
            let node = &self.nodes[id];
            let needs_buf = match node.kind {
                Kind::Sink(_) => false,
                Kind::Fifo { .. } => !node.push,
                Kind::Input => node.consumers.iter().any(|&c| matches!(self.nodes[c].kind, Kind::Sink(_))),
                _ => true,
            };
            if let Kind::Resampler { rate, .. } = &node.kind {
                let frames = if node.push { self.push_in_cap(id) } else { rate.max_in(node.cap) };
                if frames > rate.max_in_frames() {
                    return Err(Error::InvalidSize);
                }
            }
            if needs_buf && node.cap != 0 {
                let buf = Buffer::alloc(node.format, node.cap * node.channels, self.caps)?;
                self.nodes[id].buf = buf;
            }
        }
        Ok(())
    }

    /// Validates the push/pull rules and allocates.
    pub fn build(mut self) -> Result<Graph<'a>> {
        if let Some(err) = self.err {
            return Err(err);
        }
        self.link_consumers()?;
        for id in 0..self.nodes.len() {
            self.assign_direction(id)?;
        }
        if self.nodes.iter().any(|n| !n.push && n.consumers.len() > 1) {
            return Err(Error::InvalidArg);
        }
        self.size_buffers()?;
        for id in 0..self.nodes.len() {
            let generation = match &self.nodes[id].kind {
                Kind::Resampler { drift: Some(d), .. } => match &self.nodes[d.fifo].kind {
                    Kind::Fifo { output, .. } => Some(output.generation()),
                    _ => None,
                },
                _ => None,
            };
            if let (Some(generation), Kind::Resampler { drift: Some(d), .. }) = (generation, &mut self.nodes[id].kind) {
                d.generation = generation;
            }
        }
        Ok(Graph { max_frames: self.max_frames, nodes: self.nodes })
    }
}

impl Graph<'_> {
    /// The adjustment a resampler node's drift last asked for.
    pub fn drift_ppm(&self, node: NodeId) -> Option<f32> {
        match &self.nodes.get(node.0)?.kind {
            Kind::Resampler { drift: Some(d), .. } => Some(d.drift.ppm()),
            _ => None,
        }
    }

    fn gather(&mut self, id: usize, from: Option<usize>, frames: usize) -> Result<()> {
        for i in 0..self.nodes[id].srcs.len() {
            let src = self.nodes[id].srcs[i];
            if Some(src) != from {
                self.pull(src, frames)?;
            }
        }
        Ok(())
    }

    fn process(&mut self, id: usize, from: Option<(usize, Pcm<'_>)>, frames: usize) -> Result<()> {
        let mut out = mem::take(&mut self.nodes[id].buf);
        let mut inputs = mem::replace(&mut self.nodes[id].inputs, Vec::new_in(Caps::DEFAULT));
        for &src in &self.nodes[id].srcs {
            let pcm = match from {
                Some((f, data)) if f == src => data,
                _ => self.nodes[src].buf.pcm(frames * self.nodes[src].channels),
            };
            inputs.push(unsafe { mem::transmute::<Pcm<'_>, Pcm<'static>>(pcm) });
        }
        let node = &mut self.nodes[id];
        let samples = frames * node.channels;
        let result = match &mut node.kind {
            Kind::Block(block) => block.process(&inputs, out.pcm_mut(samples), frames),
            Kind::Convert => {
                match (inputs[0], out.pcm_mut(samples)) {
                    (Pcm::S16(s), PcmMut::S16(d)) => d.copy_from_slice(s),
                    (Pcm::S32(s), PcmMut::S32(d)) => d.copy_from_slice(s),
                    (Pcm::S16(s), PcmMut::S32(d)) => convert(s, d),
                    (Pcm::S32(s), PcmMut::S16(d)) => convert(s, d),
                }
                Ok(())
            }
            _ => Err(Error::InvalidState),
        };
        inputs.clear();
        let node = &mut self.nodes[id];
        node.inputs = inputs;
        node.buf = out;
        result
    }

    fn steer(&mut self, id: usize) {
        let Kind::Resampler { drift: Some(d), .. } = &self.nodes[id].kind else { return };
        let (fifo, known, elapsed) = (d.fifo, d.generation, d.elapsed);
        let Kind::Fifo { output, .. } = &self.nodes[fifo].kind else { return };
        let (generation, primed, level) = (output.generation(), output.primed(), output.level());
        let Kind::Resampler { drift: Some(d), .. } = &mut self.nodes[id].kind else { return };
        if generation != known {
            d.generation = generation;
            d.drift.reset();
            let _ = d.control.set_adjust(0.0);
        }
        if primed {
            let ppm = d.drift.update(level, elapsed);
            let _ = d.control.set_adjust(ppm);
        }
    }

    fn pull(&mut self, id: usize, frames: usize) -> Result<()> {
        let samples = frames * self.nodes[id].channels;
        let node = &mut self.nodes[id];
        match &mut node.kind {
            Kind::Source(read) => {
                let n = read(node.buf.pcm_mut(samples)).min(frames);
                node.buf.zero(n * node.channels, samples);
                Ok(())
            }
            Kind::Fifo { output, .. } => output.read(node.buf.pcm_mut(samples)),
            Kind::Resampler { .. } => {
                self.steer(id);
                let node = &mut self.nodes[id];
                let Kind::Resampler { rate, drift } = &mut node.kind else { return Err(Error::InvalidState) };
                let need = rate.frames_needed(frames);
                if let Some(d) = drift {
                    d.elapsed = need;
                }
                let src = node.srcs[0];
                self.pull(src, need)?;
                let mut out = mem::take(&mut self.nodes[id].buf);
                let input = self.nodes[src].buf.pcm(need * self.nodes[src].channels);
                let input = unsafe { mem::transmute::<Pcm<'_>, Pcm<'static>>(input) };
                let result = match &mut self.nodes[id].kind {
                    Kind::Resampler { rate, .. } => rate.process(input, out.pcm_mut(samples)),
                    _ => Err(Error::InvalidState),
                };
                let node = &mut self.nodes[id];
                node.buf = out;
                let n = result?;
                node.buf.zero(n * node.channels, samples);
                Ok(())
            }
            _ => {
                self.gather(id, None, frames)?;
                self.process(id, None, frames)
            }
        }
    }

    fn deliver(&mut self, id: usize, data: Pcm<'_>, frames: usize) -> Result<()> {
        for i in 0..self.nodes[id].consumers.len() {
            let consumer = self.nodes[id].consumers[i];
            self.feed(consumer, id, data, frames)?;
        }
        Ok(())
    }

    fn feed(&mut self, id: usize, from: usize, data: Pcm<'_>, frames: usize) -> Result<()> {
        let node = &mut self.nodes[id];
        match &mut node.kind {
            Kind::Fifo { input: Some(input), .. } => input.write(data),
            Kind::Sink(write) => write(data),
            Kind::Resampler { rate, .. } => {
                let samples = node.cap * node.channels;
                let mut out = mem::take(&mut node.buf);
                let result = rate.process(data, out.pcm_mut(samples));
                let delivered = result.and_then(|n| {
                    if n == 0 {
                        return Ok(());
                    }
                    let channels = self.nodes[id].channels;
                    self.deliver(id, out.pcm(n * channels), n)
                });
                self.nodes[id].buf = out;
                delivered
            }
            _ => {
                self.gather(id, Some(from), frames)?;
                self.process(id, Some((from, data)), frames)?;
                let out = mem::take(&mut self.nodes[id].buf);
                let channels = self.nodes[id].channels;
                let result = self.deliver(id, out.pcm(frames * channels), frames);
                self.nodes[id].buf = out;
                result
            }
        }
    }

    /// Runs the push section from `input` on the calling task and returns once
    /// the sink has taken the data.
    pub fn write<S: Sample>(&mut self, input: NodeId, data: &[S]) -> Result<()> {
        let node = self.nodes.get(input.0).ok_or(Error::InvalidArg)?;
        if !matches!(node.kind, Kind::Input) || node.format != S::FORMAT {
            return Err(Error::InvalidArg);
        }
        let id = input.0;
        let channels = node.channels;
        for chunk in data.chunks(self.max_frames * channels) {
            let frames = chunk.len() / channels;
            let mut buf = mem::take(&mut self.nodes[id].buf);
            let result = if matches!(buf, Buffer::None) {
                self.deliver(id, S::pcm(chunk), frames)
            } else {
                match (&mut buf, S::pcm(chunk)) {
                    (Buffer::S16(v), Pcm::S16(s)) => v[..s.len()].copy_from_slice(s),
                    (Buffer::S32(v), Pcm::S32(s)) => v[..s.len()].copy_from_slice(s),
                    _ => {}
                }
                self.deliver(id, buf.pcm(chunk.len()), frames)
            };
            self.nodes[id].buf = buf;
            result?;
        }
        Ok(())
    }

    /// Pulls from a pull node that has no consumer.
    pub fn read<S: Sample>(&mut self, node: NodeId, data: &mut [S]) -> Result<()> {
        let entry = self.nodes.get(node.0).ok_or(Error::InvalidArg)?;
        if entry.push || !entry.consumers.is_empty() || entry.format != S::FORMAT {
            return Err(Error::InvalidArg);
        }
        let id = node.0;
        let (channels, cap) = (entry.channels, entry.cap);
        for chunk in data.chunks_mut(cap * channels) {
            let frames = chunk.len() / channels;
            self.pull(id, frames)?;
            let pcm = S::from_pcm(self.nodes[id].buf.pcm(chunk.len())).ok_or(Error::InvalidState)?;
            chunk.copy_from_slice(pcm);
        }
        Ok(())
    }
}
