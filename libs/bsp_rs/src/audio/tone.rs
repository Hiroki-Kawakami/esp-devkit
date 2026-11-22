/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! Tones on a task of their own: straight on a buzzer, or as sine waves on the
//! stream, which stays open a moment after the last note so the next starts
//! whole.

use alloc::vec::Vec;
use core::f32::consts::TAU;
use core::sync::atomic::Ordering;
use core::time::Duration;

use devkit_core::math::{cosf, sinf};
use idf::log::warn;
use idf::sync::{Receiver, Sender, channel};
use idf::{Error, Result, thread};

use super::{BuzzerDriver, Core, Driver, Format, OPEN_FADE_MS, Pcm, SampleFormat, TAG};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Note {
    /// 0 is a rest.
    pub hz: u16,
    pub ms: u16,
}

const MAX_HZ: u16 = 20_000;
const QUEUE_DEPTH: usize = 4;
const STACK_SIZE: usize = 4096;
const PRIORITY: u32 = 5;

const RATE: u32 = 48_000;
const FORMAT: Format = Format { sample_rate: RATE, sample: SampleFormat::S16, channels: 2 };
const CHUNK_FRAMES: usize = 240;
const AMPLITUDE: f32 = 0.5 * 32767.0;
/// Rise and fall of every note, so none starts or stops on a step.
const EDGE_FRAMES: u32 = RATE * 3 / 1000;
/// Silence while the stream fades in, so the first note is heard whole.
const PREROLL_FRAMES: u32 = RATE * (OPEN_FADE_MS + 20) / 1000;
/// How long the stream stays open after the last note.
const LINGER_FRAMES: u32 = RATE;

pub(super) fn check(notes: &[Note]) -> Result<()> {
    let Some((_, init)) = notes.split_last() else { return Err(Error::INVALID_ARG) };
    if notes.iter().any(|n| n.hz > MAX_HZ) || init.iter().any(|n| n.ms == 0) {
        return Err(Error::INVALID_ARG);
    }
    Ok(())
}

enum Command {
    Play(Vec<Note>),
    /// Acknowledged once the tone is silent and the stream closed.
    Stop(Option<Sender<()>>),
}

pub(crate) struct Player(Sender<Command>);

impl Player {
    pub(super) fn start<D: Driver>(core: &'static Core<D>) -> Result<Self> {
        let (commands, queue) = channel(QUEUE_DEPTH);
        thread::Builder::new().name("audio_tone").stack_size(STACK_SIZE).priority(PRIORITY).spawn(move || run(core, &queue))?;
        Ok(Self(commands))
    }

    pub(super) fn play(&self, notes: &[Note]) {
        self.0.send(Command::Play(notes.to_vec()));
    }

    pub(super) fn stop(&self, wait: bool) {
        if !wait {
            self.0.send(Command::Stop(None));
            return;
        }
        let (done, stopped) = channel(1);
        self.0.send(Command::Stop(Some(done)));
        stopped.recv();
    }
}

fn run<D: Driver>(core: &'static Core<D>, queue: &Receiver<Command>) {
    let mut next = None;
    loop {
        match next.take().unwrap_or_else(|| queue.recv()) {
            Command::Stop(done) => {
                if let Some(buzzer) = core.driver.buzzer() {
                    log_err(buzzer.sound(0));
                }
                if let Some(done) = done {
                    done.send(());
                }
            }
            Command::Play(notes) => match core.driver.buzzer() {
                Some(buzzer) => next = buzz(buzzer, &notes, queue),
                None => sing(core, notes, queue),
            },
        }
    }
}

fn log_err(result: Result<()>) {
    if let Err(err) = result {
        warn!(TAG, "tone: {err}");
    }
}

/// Returns the command that cut the notes short.
fn buzz<B: BuzzerDriver>(buzzer: &B, notes: &[Note], queue: &Receiver<Command>) -> Option<Command> {
    for note in notes {
        log_err(buzzer.sound(u32::from(note.hz)));
        let next = if note.ms == 0 { Some(queue.recv()) } else { queue.recv_timeout(Duration::from_millis(u64::from(note.ms))) };
        if next.is_some() {
            log_err(buzzer.sound(0));
            return next;
        }
    }
    log_err(buzzer.sound(0));
    None
}

/// A note being sung: `left` is `None` while it holds.
struct Voice {
    step: f32,
    phase: f32,
    elapsed: u32,
    left: Option<u32>,
}

impl Voice {
    fn new(note: Note) -> Self {
        let left = (note.ms != 0).then(|| RATE * u32::from(note.ms) / 1000);
        Self { step: TAU * f32::from(note.hz) / RATE as f32, phase: 0.0, elapsed: 0, left }
    }

    fn release(&mut self) {
        self.left = Some(self.left.map_or(EDGE_FRAMES, |left| left.min(EDGE_FRAMES)));
    }

    fn done(&self) -> bool {
        self.left == Some(0)
    }

    /// Raised-cosine edges at both ends.
    fn envelope(&self) -> f32 {
        let edge = |frames: u32| if frames >= EDGE_FRAMES { 1.0 } else { 0.5 - 0.5 * cosf(TAU * 0.5 * frames as f32 / EDGE_FRAMES as f32) };
        edge(self.elapsed).min(self.left.map_or(1.0, edge))
    }

    fn next(&mut self) -> i16 {
        let sample = if self.step == 0.0 { 0.0 } else { sinf(self.phase) * self.envelope() * AMPLITUDE };
        self.phase += self.step;
        if self.phase >= TAU {
            self.phase -= TAU;
        }
        self.elapsed += 1;
        if let Some(left) = &mut self.left {
            *left = left.saturating_sub(1);
        }
        sample as i16
    }
}

/// Writes without pause from the stream's start to its close: a gap would
/// make the output drop what it holds and click.
fn sing<D: Driver>(core: &Core<D>, notes: Vec<Note>, queue: &Receiver<Command>) {
    core.tone_streaming.store(true, Ordering::Release);
    let mut stream = match core.open(FORMAT) {
        Ok(stream) => stream,
        Err(err) => {
            core.tone_streaming.store(false, Ordering::Release);
            warn!(TAG, "tone stream: {err}");
            return;
        }
    };
    let mut buf = [0i16; CHUNK_FRAMES * 2];
    let mut notes = notes.into_iter();
    let mut voice: Option<Voice> = None;
    let mut silence = PREROLL_FRAMES;
    let mut linger = LINGER_FRAMES;
    let mut stopping = None;
    loop {
        match queue.try_recv() {
            Some(Command::Play(next)) => {
                notes = next.into_iter();
                linger = LINGER_FRAMES;
                if let Some(voice) = &mut voice {
                    voice.release();
                }
            }
            Some(Command::Stop(done)) => {
                notes = Vec::new().into_iter();
                stopping = Some(done);
                if let Some(voice) = &mut voice {
                    voice.release();
                }
            }
            None => {}
        }
        if voice.as_ref().is_none_or(Voice::done) && silence == 0 {
            voice = notes.next().map(Voice::new);
        }
        if voice.is_none() && silence == 0 {
            if stopping.is_some() || linger == 0 {
                break;
            }
            silence = (CHUNK_FRAMES as u32).min(linger);
            linger -= silence;
        }
        let mut frames = 0;
        for frame in buf.chunks_exact_mut(2) {
            let sample = match &mut voice {
                Some(v) if silence == 0 && !v.done() => v.next(),
                _ => {
                    silence = silence.saturating_sub(1);
                    0
                }
            };
            frame.fill(sample);
            frames += 1;
            if silence == 0 && voice.as_ref().is_some_and(Voice::done) {
                break;
            }
        }
        if let Err(err) = stream.write(Pcm::S16(&buf[..frames * 2])) {
            warn!(TAG, "tone write: {err}");
            break;
        }
    }
    log_err(stream.close());
    core.tone_streaming.store(false, Ordering::Release);
    if let Some(done) = stopping.flatten() {
        done.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(voice: &mut Voice) -> Vec<i16> {
        let mut out = Vec::new();
        while !voice.done() {
            out.push(voice.next());
        }
        out
    }

    #[test]
    fn checks_notes() {
        let n = |hz, ms| Note { hz, ms };
        assert!(check(&[]).is_err());
        assert!(check(&[n(440, 100)]).is_ok());
        assert!(check(&[n(440, 100), n(0, 50), n(880, 0)]).is_ok());
        assert!(check(&[n(440, 0), n(880, 100)]).is_err());
        assert!(check(&[n(MAX_HZ + 1, 100)]).is_err());
    }

    #[test]
    fn note_lasts_its_length_and_starts_and_ends_quiet() {
        let samples = render(&mut Voice::new(Note { hz: 1000, ms: 50 }));
        assert_eq!(samples.len() as u32, RATE * 50 / 1000);
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(f32::from(peak) > AMPLITUDE * 0.99);
        let edge = (AMPLITUDE * 0.05) as u16;
        assert!(samples[..4].iter().all(|s| s.unsigned_abs() < edge));
        assert!(samples[samples.len() - 4..].iter().all(|s| s.unsigned_abs() < edge));
    }

    #[test]
    fn steps_stay_small() {
        let samples = render(&mut Voice::new(Note { hz: 2000, ms: 30 }));
        let max_step = TAU * 2000.0 / RATE as f32 * AMPLITUDE * 1.05;
        assert!(samples.windows(2).all(|w| f32::from((w[1] - w[0]).unsigned_abs()) <= max_step));
    }

    #[test]
    fn rest_is_silent() {
        let samples = render(&mut Voice::new(Note { hz: 0, ms: 20 }));
        assert_eq!(samples.len() as u32, RATE * 20 / 1000);
        assert!(samples.iter().all(|&s| s == 0));
    }

    #[test]
    fn held_note_fades_out_on_release() {
        let mut voice = Voice::new(Note { hz: 440, ms: 0 });
        for _ in 0..RATE / 10 {
            voice.next();
        }
        assert!(!voice.done());
        voice.release();
        let tail = render(&mut voice);
        assert_eq!(tail.len() as u32, EDGE_FRAMES);
        assert!(tail.last().unwrap().unsigned_abs() < (AMPLITUDE * 0.05) as u16);
    }
}
