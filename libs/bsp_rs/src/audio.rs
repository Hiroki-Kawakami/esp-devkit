/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use devkit::math::powf;
use devkit_audio::convert::{self, PcmLayout};
use devkit_audio::eq::{Biquad, Eq, EqConfig, EqControl};
use devkit_audio::gain::{Gain, GainConfig, GainControl};
use devkit_audio::mixer::{Mixer, MixerConfig, MixerControl};
pub use devkit_audio::{Format as SampleFormat, Pcm};
use devkit_audio::Sample;
use idf::heap::{self, Caps};
use idf::log::{error, warn};
use idf::sync::{Mutex, OnceLock};
use idf::time::Instant;
use idf::{Error, Result, thread};

use crate::Absent;
use crate::dispatch::{self, Next, Notifier, Source};

const TAG: &CStr = c"audio";

pub const VOLUME_MAX: u8 = 150;
const VOLUME_UNITY: u8 = 100;
const VOLUME_DB_SPAN: f32 = 40.0;
const VOLUME_BOOST_DB: f32 = 6.0;

const NOMINAL_RATE: u32 = 48_000;
const VOLUME_FADE_MS: u32 = 100;
const OPEN_FADE_MS: u32 = 50;
// The DAC's analog stage settles before the amp is first enabled.
const AMP_SETTLE: Duration = Duration::from_millis(50);
const ROUTE_POLL: Duration = Duration::from_millis(200);
const CHUNK_FRAMES: usize = 256;
const MAX_CHANNELS: usize = 2;
const MAX_SAMPLE_BYTES: usize = 4;

pub(crate) const MAX_STAGES: usize = 8;

const MATRIX_STEREO: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const MATRIX_MONO: [f32; 4] = [0.5, 0.5, 0.5, 0.5];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Format {
    pub sample_rate: u32,
    pub sample: SampleFormat,
    /// 1 or 2.
    pub channels: u8,
}

impl Format {
    fn is_valid(&self) -> bool {
        self.sample_rate > 0 && (1..=MAX_CHANNELS as u8).contains(&self.channels)
    }

    fn layout(&self) -> PcmLayout {
        match self.sample {
            SampleFormat::S16 => PcmLayout::S16,
            SampleFormat::S32 => PcmLayout::S32,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum SpeakerMode {
    #[default]
    On,
    /// On only while no headphone is inserted.
    Auto,
    Off,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum DspMode {
    /// The board's tuning, applied again on every route change.
    #[default]
    Auto,
    /// Starts flat; the app drives [`Audio::eq`] and [`Audio::mixer`].
    Manual,
    /// No DSP. Volume and mute move on the hardware and may click, and volume
    /// needs a hardware gain.
    Disable,
}

#[derive(Default)]
#[non_exhaustive]
pub struct Config {
    pub dsp_mode: DspMode,
    pub speaker_mode: SpeakerMode,
}

pub type HeadphoneCallback = Box<dyn FnMut(bool) + Send>;

#[allow(dead_code)]
pub(crate) enum HwMute {
    None,
    Step,
    /// Silent after this long, at the rate playing now.
    Ramp(Duration),
}

/// A board's tuning for one output route.
pub(crate) struct DspProfile {
    pub biquads: [Biquad; MAX_STAGES],
    pub stages: usize,
    pub eq_enabled: bool,
    pub mono_mix: bool,
}

/// The closed provider holds the output muted and the amp off.
pub(crate) trait Driver: Send + Sync + 'static {
    /// Also changes the format of a running stream. The hardware mute stays set.
    fn open(&self, format: Format) -> Result<()>;
    fn close(&self) -> Result<()>;
    /// Blocks while the output is full.
    fn write(&self, data: &[u8]) -> Result<()>;

    /// How long the output stays up after a stream ends, for a provider whose
    /// open or close is audible: a stream of the same format starting within
    /// this time skips both.
    fn close_delay(&self) -> Duration {
        Duration::ZERO
    }

    /// Blocks until everything written has played and leaves silence behind.
    fn drain(&self) -> Result<()> {
        Ok(())
    }

    /// Frames written that have not played yet.
    fn queued(&self) -> Result<usize> {
        Err(Error::NOT_SUPPORTED)
    }

    /// The bottom of the hardware gain, which tops out at 0 dB.
    fn hw_gain_min_db(&self) -> Option<f32> {
        None
    }

    fn set_hw_gain(&self, _db: f32) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn hw_mute(&self) -> HwMute {
        HwMute::None
    }

    /// Does not wait for a ramp.
    fn set_hw_mute(&self, _mute: bool) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn has_speaker(&self) -> bool {
        false
    }

    fn set_speaker_enabled(&self, _enabled: bool) -> Result<()> {
        Err(Error::NOT_SUPPORTED)
    }

    fn has_headphone(&self) -> bool {
        false
    }

    fn headphone_inserted(&self) -> Result<bool> {
        Err(Error::NOT_SUPPORTED)
    }

    fn dsp_profile(&self, _headphone: bool, _sample_rate: u32) -> Option<DspProfile> {
        None
    }
}

impl Driver for Absent {
    fn open(&self, _format: Format) -> Result<()> {
        match *self {}
    }

    fn close(&self) -> Result<()> {
        match *self {}
    }

    fn write(&self, _data: &[u8]) -> Result<()> {
        match *self {}
    }
}

fn map_err(err: devkit_audio::Error) -> Error {
    match err {
        devkit_audio::Error::NoMemory => Error::NO_MEM,
        devkit_audio::Error::NotSupported => Error::NOT_SUPPORTED,
        devkit_audio::Error::InvalidState => Error::INVALID_STATE,
        _ => Error::INVALID_ARG,
    }
}

fn volume_db(volume: u8) -> f32 {
    if volume == 0 {
        return f32::NEG_INFINITY;
    }
    let volume = f32::from(volume.min(VOLUME_MAX));
    let unity = f32::from(VOLUME_UNITY);
    if volume <= unity {
        (volume - unity) * (VOLUME_DB_SPAN / unity)
    } else {
        (volume - unity) * (VOLUME_BOOST_DB / (f32::from(VOLUME_MAX) - unity))
    }
}

fn volume_gain(volume: u8) -> f32 {
    if volume == 0 { 0.0 } else { powf(10.0, volume_db(volume) / 20.0) }
}

struct Controls {
    eq: EqControl,
    gain: GainControl,
    mixer: MixerControl,
}

// S32 rather than S16 in place: an EQ boost would saturate before the volume
// gain brings it back down.
struct Dsp {
    eq: Eq<i32>,
    gain: Gain<i32>,
    mixer: Mixer<i32>,
    work: Vec<i32, Caps>,
}

struct Pipeline {
    dsp: Option<Dsp>,
    out: Vec<u8, Caps>,
}

fn alloc_filled<T: Clone>(len: usize, value: T) -> Result<Vec<T, Caps>> {
    let mut vec = heap::try_vec(len, Caps::SPIRAM).or_else(|_| heap::try_vec(len, Caps::DEFAULT))?;
    vec.resize(len, value);
    Ok(vec)
}

fn create_dsp() -> Result<(Controls, Dsp)> {
    let (eq, eq_control) =
        Eq::new(&EqConfig { max_stages: MAX_STAGES, enabled: true, ..EqConfig::default() }).map_err(map_err)?;
    let (gain, gain_control) = Gain::new(&GainConfig { gain: 0.0, ..GainConfig::default() }).map_err(map_err)?;
    let (mixer, mixer_control) = Mixer::new(&MixerConfig {
        out_channels: MAX_CHANNELS as u8,
        in_channels: &[MAX_CHANNELS as u8],
        caps: Caps::DEFAULT,
    })
    .map_err(map_err)?;
    let work = alloc_filled(CHUNK_FRAMES * MAX_CHANNELS, 0)?;
    Ok((
        Controls { eq: eq_control, gain: gain_control, mixer: mixer_control },
        Dsp { eq, gain, mixer, work },
    ))
}

struct State {
    volume: u8,
    mute: bool,
    eq_override: Option<bool>,
    speaker_mode: SpeakerMode,
    // The DAC has produced settled silence, so the amp may follow the mode.
    armed: bool,
    rate: u32,
    // Open from a stream's first write to its end. Separate from the user's
    // mute: it covers the moments a software fade cannot, because nothing is
    // being written.
    gate_open: bool,
    held: Option<Held>,
}

// The provider is still open after its stream ended.
struct Held {
    format: Format,
    until: Instant,
}

struct Core<D> {
    driver: D,
    dsp_mode: DspMode,
    controls: Option<Controls>,
    tracks_route: bool,
    pipeline: Mutex<Option<Pipeline>>,
    state: Mutex<State>,
    claimed: AtomicBool,
    zero_pcm: AtomicBool,
    callback: Mutex<Option<Option<HeadphoneCallback>>>,
    callback_changed: AtomicBool,
    notifier: OnceLock<Notifier>,
}

impl<D: Driver> Core<D> {
    fn new(driver: D, config: &Config) -> Result<Self> {
        let (controls, dsp) = match config.dsp_mode {
            DspMode::Disable => (None, None),
            DspMode::Auto | DspMode::Manual => match create_dsp() {
                Ok((controls, dsp)) => (Some(controls), Some(dsp)),
                Err(err) => {
                    warn!(TAG, "dsp unavailable: {err}");
                    (None, None)
                }
            },
        };
        let out = alloc_filled(CHUNK_FRAMES * MAX_CHANNELS * MAX_SAMPLE_BYTES, 0)?;
        let mut audio = Self {
            driver,
            dsp_mode: config.dsp_mode,
            controls,
            tracks_route: false,
            pipeline: Mutex::try_new(Some(Pipeline { dsp, out }))?,
            state: Mutex::try_new(State {
                volume: 0,
                mute: false,
                eq_override: None,
                speaker_mode: config.speaker_mode,
                armed: false,
                rate: NOMINAL_RATE,
                gate_open: false,
                held: None,
            })?,
            claimed: AtomicBool::new(false),
            zero_pcm: AtomicBool::new(false),
            callback: Mutex::try_new(None)?,
            callback_changed: AtomicBool::new(false),
            notifier: OnceLock::new(),
        };
        let voiced = {
            let state = audio.state.lock();
            let voiced = audio.apply_profile(&state, audio.headphone_inserted());
            log_err("hardware output", audio.apply_output(&state));
            voiced
        };
        audio.tracks_route = voiced;
        Ok(audio)
    }

    fn open(&self, format: Format) -> Result<StreamCore<'_, D>> {
        if !format.is_valid() {
            return Err(Error::INVALID_ARG);
        }
        if self.claimed.swap(true, Ordering::AcqRel) {
            return Err(Error::INVALID_STATE);
        }
        let pipeline = self.pipeline.lock().take();
        let mut stream = StreamCore { audio: self, format, pipeline, running: false, gate_pending: false };
        let held = self.state.lock().held.take();
        if held.is_none_or(|held| held.format != format) {
            self.driver.open(format)?;
        }
        stream.running = true;
        stream.start_output();
        Ok(stream)
    }

    fn set_volume(&self, volume: u8) -> Result<()> {
        if self.controls.is_none() && self.driver.hw_gain_min_db().is_none() {
            return Err(Error::NOT_SUPPORTED);
        }
        let mut state = self.state.lock();
        let volume = volume.min(VOLUME_MAX);
        if volume == state.volume {
            return Ok(());
        }
        state.volume = volume;
        self.apply_level(&state)
    }

    fn volume(&self) -> u8 {
        self.state.lock().volume
    }

    fn set_mute(&self, mute: bool) -> Result<()> {
        let mut state = self.state.lock();
        if mute == state.mute {
            return Ok(());
        }
        state.mute = mute;
        self.apply_level(&state)
    }

    fn mute(&self) -> bool {
        self.state.lock().mute
    }

    fn eq(&self) -> Option<&EqControl> {
        self.controls.as_ref().map(|c| &c.eq)
    }

    fn mixer(&self) -> Option<&MixerControl> {
        self.controls.as_ref().map(|c| &c.mixer)
    }

    fn set_eq_enabled(&self, enabled: bool) -> Result<()> {
        let controls = self.controls.as_ref().ok_or(Error::NOT_SUPPORTED)?;
        self.state.lock().eq_override = Some(enabled);
        controls.eq.set_enabled(enabled);
        Ok(())
    }

    fn eq_enabled(&self) -> bool {
        self.controls.as_ref().is_some_and(|c| c.eq.enabled())
    }

    fn set_speaker_mode(&self, mode: SpeakerMode) -> Result<()> {
        if !self.driver.has_speaker() || (mode == SpeakerMode::Auto && !self.driver.has_headphone()) {
            return Err(Error::NOT_SUPPORTED);
        }
        self.state.lock().speaker_mode = mode;
        match self.notifier.get() {
            Some(notifier) => notifier.notify(),
            None => self.apply_speaker(self.headphone_inserted()),
        }
        Ok(())
    }

    fn speaker_mode(&self) -> SpeakerMode {
        self.state.lock().speaker_mode
    }

    fn headphone_inserted(&self) -> bool {
        if !self.driver.has_headphone() {
            return false;
        }
        self.driver.headphone_inserted().unwrap_or_else(|err| {
            warn!(TAG, "headphone detect: {err}");
            false
        })
    }

    fn set_headphone_callback(&self, callback: Option<HeadphoneCallback>) -> Result<()> {
        if !self.driver.has_headphone() {
            return Err(Error::NOT_SUPPORTED);
        }
        *self.callback.lock() = Some(callback);
        self.callback_changed.store(true, Ordering::Release);
        if let Some(notifier) = self.notifier.get() {
            notifier.notify();
        }
        Ok(())
    }

    fn apply_level(&self, state: &State) -> Result<()> {
        match &self.controls {
            Some(controls) => {
                if state.gate_open {
                    controls.gain.set(state.gain(), VOLUME_FADE_MS);
                }
                Ok(())
            }
            None => self.apply_output(state),
        }
    }

    fn apply_output(&self, state: &State) -> Result<()> {
        let gain_min = self.driver.hw_gain_min_db();
        let hw_mute = self.driver.hw_mute();
        let has_hw_mute = !matches!(hw_mute, HwMute::None);
        let no_dsp = self.controls.is_none();
        let silenced = no_dsp && (state.mute || (gain_min.is_some() && state.volume == 0));

        if has_hw_mute {
            self.driver.set_hw_mute(!state.gate_open || silenced)?;
        }
        if no_dsp && let Some(min) = gain_min {
            let db = if silenced && !has_hw_mute { min } else { volume_db(state.volume).clamp(min, 0.0) };
            self.driver.set_hw_gain(db)?;
        }
        self.zero_pcm.store(silenced && !has_hw_mute && gain_min.is_none(), Ordering::Relaxed);
        Ok(())
    }

    fn apply_profile(&self, state: &State, headphone: bool) -> bool {
        let Some(controls) = &self.controls else { return false };
        if self.dsp_mode != DspMode::Auto {
            return false;
        }
        let Some(profile) = self.driver.dsp_profile(headphone, state.rate) else { return false };
        let biquads = &profile.biquads[..profile.stages.min(MAX_STAGES)];
        if let Err(err) = controls.eq.set_biquads(biquads) {
            warn!(TAG, "set biquads: {err}");
        }
        controls.eq.set_enabled(state.eq_override.unwrap_or(profile.eq_enabled));
        let matrix = if profile.mono_mix { &MATRIX_MONO } else { &MATRIX_STEREO };
        if let Err(err) = controls.mixer.set_matrix(0, matrix) {
            warn!(TAG, "set matrix: {err}");
        }
        true
    }

    // The state stays locked across the close, so an open waits for it rather
    // than running into it.
    fn expire_hold(&self) -> Option<Duration> {
        let mut state = self.state.lock();
        let until = state.held.as_ref()?.until;
        if let Some(left) = until.checked_duration_since(Instant::now())
            && !left.is_zero()
        {
            return Some(left);
        }
        state.held = None;
        log_err("close", self.driver.close());
        None
    }

    fn apply_speaker(&self, headphone: bool) {
        if !self.driver.has_speaker() {
            return;
        }
        let state = self.state.lock();
        let desired = match state.speaker_mode {
            SpeakerMode::On => true,
            SpeakerMode::Auto => !headphone,
            SpeakerMode::Off => false,
        };
        log_err("amp", self.driver.set_speaker_enabled(desired && state.armed));
    }
}

impl State {
    fn gain(&self) -> f32 {
        if self.mute { 0.0 } else { volume_gain(self.volume) }
    }
}

fn log_err(what: &str, result: Result<()>) {
    if let Err(err) = result {
        warn!(TAG, "{what}: {err}");
    }
}

struct StreamCore<'a, D: Driver> {
    audio: &'a Core<D>,
    format: Format,
    pipeline: Option<Pipeline>,
    running: bool,
    gate_pending: bool,
}

impl<D: Driver> StreamCore<'_, D> {
    fn write(&mut self, pcm: Pcm<'_>) -> Result<()> {
        if !self.running {
            return Err(Error::INVALID_STATE);
        }
        if pcm.format() != self.format.sample || pcm.len() % usize::from(self.format.channels) != 0 {
            return Err(Error::INVALID_ARG);
        }
        if pcm.is_empty() {
            return Ok(());
        }
        if self.gate_pending {
            self.open_gate();
        }
        match pcm {
            Pcm::S16(samples) => self.write_samples(samples),
            Pcm::S32(samples) => self.write_samples(samples),
        }
    }

    // Each chunk is written as soon as it is processed: a provider may drop a
    // partly filled DMA buffer when its next write comes late, and processing a
    // whole long buffer before writing any of it is late enough.
    fn write_samples<S: Sample>(&mut self, samples: &[S]) -> Result<()> {
        let channels = usize::from(self.format.channels);
        let layout = self.format.layout();
        let driver = &self.audio.driver;
        let zero_pcm = self.audio.zero_pcm.load(Ordering::Relaxed);
        let Pipeline { dsp, out } = self.pipeline.as_mut().ok_or(Error::INVALID_STATE)?;
        for chunk in samples.chunks(CHUNK_FRAMES * channels) {
            let out = &mut out[..chunk.len() * layout.bytes()];
            match dsp {
                Some(dsp) => {
                    let work = &mut dsp.work[..chunk.len()];
                    convert::convert(chunk, work);
                    dsp.eq.process_in_place(work);
                    dsp.gain.process_in_place(work);
                    if channels == MAX_CHANNELS {
                        dsp.mixer.process_in_place(work, &[]);
                    }
                    convert::to_pcm(work, out, layout);
                }
                None if zero_pcm => out.fill(0),
                None => convert::to_pcm(chunk, out, layout),
            }
            driver.write(out)?;
        }
        Ok(())
    }

    fn queued(&self) -> Result<usize> {
        if !self.running {
            return Err(Error::INVALID_STATE);
        }
        self.audio.driver.queued()
    }

    fn reconfigure(&mut self, format: Format) -> Result<()> {
        if !format.is_valid() {
            return Err(Error::INVALID_ARG);
        }
        if self.running {
            if format == self.format {
                return Ok(());
            }
            self.stop_output();
            self.running = false;
        }
        self.audio.driver.open(format)?;
        self.format = format;
        self.running = true;
        self.start_output();
        Ok(())
    }

    fn close(mut self) -> Result<()> {
        self.stop()
    }

    fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }
        self.stop_output();
        self.running = false;
        let audio = self.audio;
        let delay = audio.driver.close_delay();
        if delay.is_zero() {
            return audio.driver.close();
        }
        audio.state.lock().held = Some(Held { format: self.format, until: Instant::now() + delay });
        if let Some(notifier) = audio.notifier.get() {
            notifier.notify();
        }
        Ok(())
    }

    fn start_output(&mut self) {
        let audio = self.audio;
        let Format { sample_rate, channels, .. } = self.format;
        if let Some(Pipeline { dsp: Some(dsp), .. }) = &mut self.pipeline {
            log_err("eq", dsp.eq.reconfig(channels).map_err(map_err));
            log_err("gain", dsp.gain.reconfig(channels, sample_rate).map_err(map_err));
        }
        let headphone = audio.headphone_inserted();
        let mut state = audio.state.lock();
        state.rate = sample_rate;
        audio.apply_profile(&state, headphone);
        if audio.controls.is_some() && audio.driver.hw_gain_min_db().is_some() {
            log_err("hardware gain", audio.driver.set_hw_gain(0.0));
        }
        let first = !state.armed;
        drop(state);

        if first {
            thread::sleep(AMP_SETTLE);
            audio.state.lock().armed = true;
            audio.apply_speaker(headphone);
        }
        self.gate_pending = true;
    }

    // Waits for the first write, so the hardware unmutes after the amp is up and
    // the software fade starts on the first sample.
    fn open_gate(&mut self) {
        self.gate_pending = false;
        let audio = self.audio;
        let mut state = audio.state.lock();
        state.gate_open = true;
        if let Some(controls) = &audio.controls {
            controls.gain.set(0.0, 0);
            controls.gain.set(state.gain(), OPEN_FADE_MS);
        }
        log_err("hardware output", audio.apply_output(&state));
    }

    fn stop_output(&mut self) {
        let audio = self.audio;
        log_err("drain", audio.driver.drain());
        {
            let mut state = audio.state.lock();
            state.gate_open = false;
            log_err("hardware output", audio.apply_output(&state));
        }
        if let HwMute::Ramp(time) = audio.driver.hw_mute() {
            thread::sleep(time);
        }
    }
}

impl<D: Driver> Drop for StreamCore<'_, D> {
    fn drop(&mut self) {
        if let Err(err) = self.stop() {
            error!(TAG, "close: {err}");
        }
        *self.audio.pipeline.lock() = self.pipeline.take();
        self.audio.claimed.store(false, Ordering::Release);
    }
}

struct OutputSource<'a, D> {
    audio: &'a Core<D>,
    callback: Option<HeadphoneCallback>,
    last: Option<bool>,
    failing: bool,
}

impl<D: Driver> OutputSource<'_, D> {
    fn detect(&mut self) -> bool {
        match self.audio.driver.headphone_inserted() {
            Ok(headphone) => {
                self.failing = false;
                headphone
            }
            Err(err) => {
                if !self.failing {
                    warn!(TAG, "headphone detect: {err}");
                    self.failing = true;
                }
                false
            }
        }
    }

    fn poll(&mut self) -> Next {
        let audio = self.audio;
        if audio.callback_changed.swap(false, Ordering::Acquire)
            && let Some(callback) = audio.callback.lock().take()
        {
            self.callback = callback;
        }

        let headphone = audio.driver.has_headphone() && self.detect();
        let changed = self.last.is_some_and(|last| last != headphone);
        self.last = Some(headphone);
        if changed {
            audio.apply_profile(&audio.state.lock(), headphone);
            if let Some(callback) = &mut self.callback {
                callback(headphone);
            }
        }
        audio.apply_speaker(headphone);

        let held = audio.expire_hold();
        let auto = audio.state.lock().speaker_mode == SpeakerMode::Auto;
        let tracking = audio.driver.has_headphone() && (auto || self.callback.is_some() || audio.tracks_route);
        match (tracking.then_some(ROUTE_POLL), held) {
            (Some(route), Some(held)) => Next::After(route.min(held)),
            (Some(next), None) | (None, Some(next)) => Next::After(next),
            (None, None) => Next::Idle,
        }
    }
}

impl<D: Driver> Source for OutputSource<'static, D> {
    fn tick(&mut self) -> Next {
        self.poll()
    }
}

/// Volume, mute, routing and EQ, from any task.
pub struct Audio(Core<crate::board::AudioDriver>);

impl Audio {
    /// Starts the only stream there can be: `INVALID_STATE` while another
    /// exists. It fades in from silence.
    pub fn open(&self, format: Format) -> Result<Stream<'_>> {
        self.0.open(format).map(Stream)
    }

    /// `0..=VOLUME_MAX`, linear in dB: 0 is silence, 100 unity, and above that
    /// a digital boost only the DSP can give. Starts at 0. Without a DSP it
    /// needs a hardware gain, and may click.
    pub fn set_volume(&self, volume: u8) -> Result<()> {
        self.0.set_volume(volume)
    }

    pub fn volume(&self) -> u8 {
        self.0.volume()
    }

    pub fn set_mute(&self, mute: bool) -> Result<()> {
        self.0.set_mute(mute)
    }

    pub fn mute(&self) -> bool {
        self.0.mute()
    }

    /// `None` without a DSP. In [`DspMode::Auto`] a route change replaces its
    /// biquads; [`Self::set_eq_enabled`] survives that.
    pub fn eq(&self) -> Option<&EqControl> {
        self.0.eq()
    }

    /// `None` without a DSP. Applies to stereo streams only, and in
    /// [`DspMode::Auto`] a route change replaces its matrix.
    pub fn mixer(&self) -> Option<&MixerControl> {
        self.0.mixer()
    }

    pub fn set_eq_enabled(&self, enabled: bool) -> Result<()> {
        self.0.set_eq_enabled(enabled)
    }

    pub fn eq_enabled(&self) -> bool {
        self.0.eq_enabled()
    }

    pub fn set_speaker_mode(&self, mode: SpeakerMode) -> Result<()> {
        self.0.set_speaker_mode(mode)
    }

    pub fn speaker_mode(&self) -> SpeakerMode {
        self.0.speaker_mode()
    }

    pub fn headphone_inserted(&self) -> bool {
        self.0.headphone_inserted()
    }

    /// Runs on the dispatch task on every insert and removal, within about
    /// 200 ms. The previous callback may still run once after this returns.
    pub fn set_headphone_callback(&self, callback: Option<HeadphoneCallback>) -> Result<()> {
        self.0.set_headphone_callback(callback)
    }
}

/// The playing stream. Dropping it closes it as [`Self::close`] does.
pub struct Stream<'a>(StreamCore<'a, crate::board::AudioDriver>);

impl Stream<'_> {
    pub fn format(&self) -> Format {
        self.0.format
    }

    /// Whole frames in the stream's sample format. Blocks while the output is
    /// full, which paces the caller. A gap of more than about 960 frames between
    /// writes makes the driver drop its partial buffer, which plays as zeros and
    /// clicks.
    pub fn write(&mut self, pcm: Pcm<'_>) -> Result<()> {
        self.0.write(pcm)
    }

    /// Frames written that have not played yet: the next write is heard after
    /// that many.
    pub fn queued(&self) -> Result<usize> {
        self.0.queued()
    }

    /// Plays out what was written, then fades the new format in. A no-op when
    /// `format` is the current one. After a failure the stream takes no writes
    /// until a later call succeeds.
    pub fn reconfigure(&mut self, format: Format) -> Result<()> {
        self.0.reconfigure(format)
    }

    /// Plays out what was written, then stops. The output may stay up for a
    /// moment, so that a next stream of the same format starts without the
    /// click of powering it.
    pub fn close(self) -> Result<()> {
        self.0.close()
    }
}

static AUDIO: OnceLock<Audio> = OnceLock::new();

pub(crate) fn start(driver: crate::board::AudioDriver, config: &Config) -> Result<&'static Audio> {
    let audio = AUDIO.get_or_try_init(|| Core::new(driver, config).map(Audio))?;
    let core = &audio.0;
    match dispatch::add(Box::new(OutputSource { audio: core, callback: None, last: None, failing: false })) {
        Ok(notifier) => {
            let _ = core.notifier.set(notifier);
        }
        Err(err) => warn!(TAG, "output tracking unavailable: {err}"),
    }
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec;
    use core::sync::atomic::AtomicUsize;
    use std::sync::Mutex as StdMutex;

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Call {
        Open,
        Close,
        Drain,
        Gain(f32),
        Mute(bool),
        Amp(bool),
    }

    #[derive(Clone, Copy)]
    enum Mute {
        None,
        Step,
        Ramp,
    }

    struct Shared {
        calls: StdMutex<Vec<Call>>,
        bytes: StdMutex<Vec<u8>>,
        headphone: AtomicBool,
        profiles: AtomicUsize,
    }

    struct Stub {
        shared: Arc<Shared>,
        gain: bool,
        mute: Mute,
        profile: bool,
        hold: Duration,
    }

    impl Stub {
        fn new(gain: bool, mute: Mute, profile: bool) -> (Self, Arc<Shared>) {
            let shared = Arc::new(Shared {
                calls: StdMutex::new(Vec::new()),
                bytes: StdMutex::new(Vec::new()),
                headphone: AtomicBool::new(false),
                profiles: AtomicUsize::new(0),
            });
            (Self { shared: shared.clone(), gain, mute, profile, hold: Duration::ZERO }, shared)
        }

        fn record(&self, call: Call) -> Result<()> {
            self.shared.calls.lock().unwrap().push(call);
            Ok(())
        }
    }

    impl Shared {
        fn take_calls(&self) -> Vec<Call> {
            core::mem::take(&mut self.calls.lock().unwrap())
        }

        fn take_samples(&self) -> Vec<i16> {
            let bytes = core::mem::take(&mut *self.bytes.lock().unwrap());
            bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
        }
    }

    impl Driver for Stub {
        fn open(&self, _format: Format) -> Result<()> {
            self.record(Call::Open)
        }

        fn close(&self) -> Result<()> {
            self.record(Call::Close)
        }

        fn write(&self, data: &[u8]) -> Result<()> {
            self.shared.bytes.lock().unwrap().extend_from_slice(data);
            Ok(())
        }

        fn close_delay(&self) -> Duration {
            self.hold
        }

        fn drain(&self) -> Result<()> {
            self.record(Call::Drain)
        }

        fn queued(&self) -> Result<usize> {
            Ok(12)
        }

        fn hw_gain_min_db(&self) -> Option<f32> {
            self.gain.then_some(-96.0)
        }

        fn set_hw_gain(&self, db: f32) -> Result<()> {
            self.record(Call::Gain(db))
        }

        fn hw_mute(&self) -> HwMute {
            match self.mute {
                Mute::None => HwMute::None,
                Mute::Step => HwMute::Step,
                Mute::Ramp => HwMute::Ramp(Duration::from_millis(1)),
            }
        }

        fn set_hw_mute(&self, mute: bool) -> Result<()> {
            self.record(Call::Mute(mute))
        }

        fn has_speaker(&self) -> bool {
            true
        }

        fn set_speaker_enabled(&self, enabled: bool) -> Result<()> {
            self.record(Call::Amp(enabled))
        }

        fn has_headphone(&self) -> bool {
            true
        }

        fn headphone_inserted(&self) -> Result<bool> {
            Ok(self.shared.headphone.load(Ordering::SeqCst))
        }

        fn dsp_profile(&self, headphone: bool, sample_rate: u32) -> Option<DspProfile> {
            if !self.profile {
                return None;
            }
            self.shared.profiles.fetch_add(1, Ordering::SeqCst);
            let mut biquads = [Biquad::default(); MAX_STAGES];
            biquads[0] = Biquad::peaking(sample_rate, 1000.0, 1.0, 6.0);
            Some(DspProfile { biquads, stages: 1, eq_enabled: true, mono_mix: !headphone })
        }
    }

    const STEREO: Format = Format { sample_rate: 48_000, sample: SampleFormat::S16, channels: 2 };
    const FADE_SAMPLES: usize = 4800;

    fn audio_with(driver: Stub, dsp_mode: DspMode) -> Core<Stub> {
        Core::new(driver, &Config { dsp_mode, speaker_mode: SpeakerMode::On }).unwrap()
    }

    fn route(audio: &Core<Stub>) -> OutputSource<'_, Stub> {
        OutputSource { audio, callback: None, last: None, failing: false }
    }

    #[test]
    fn volume_curve() {
        assert_eq!(volume_gain(0), 0.0);
        assert!((volume_gain(100) - 1.0).abs() < 1e-5);
        assert!((volume_db(1) + 39.6).abs() < 1e-3);
        assert!((volume_db(50) + 20.0).abs() < 1e-3);
        assert!((volume_db(150) - 6.0).abs() < 1e-3);
    }

    #[test]
    fn gate_opens_on_the_first_write_with_the_amp_already_up() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        assert_eq!(shared.take_calls(), [Call::Mute(true)]);

        let mut stream = audio.open(STEREO).unwrap();
        assert_eq!(shared.take_calls(), [Call::Open, Call::Gain(0.0), Call::Amp(true)]);

        stream.write(Pcm::S16(&[0, 0])).unwrap();
        stream.write(Pcm::S16(&[0, 0])).unwrap();
        assert_eq!(shared.take_calls(), [Call::Mute(false)]);

        stream.close().unwrap();
        assert_eq!(shared.take_calls(), [Call::Drain, Call::Mute(true), Call::Close]);
    }

    #[test]
    fn amp_stays_off_until_the_first_open() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        let mut source = route(&audio);
        source.poll();
        assert_eq!(shared.take_calls(), [Call::Mute(true), Call::Amp(false)]);
        drop(audio.open(STEREO).unwrap());
        shared.take_calls();
        source.poll();
        assert_eq!(shared.take_calls(), [Call::Amp(true)]);
    }

    #[test]
    fn only_one_stream_at_a_time() {
        let (driver, _shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        let stream = audio.open(STEREO).unwrap();
        assert_eq!(audio.open(STEREO).err(), Some(Error::INVALID_STATE));
        drop(stream);
        assert!(audio.open(STEREO).is_ok());
    }

    #[test]
    fn invalid_format_is_refused() {
        let (driver, _shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        assert_eq!(audio.open(Format { channels: 3, ..STEREO }).err(), Some(Error::INVALID_ARG));
        assert_eq!(audio.open(Format { sample_rate: 0, ..STEREO }).err(), Some(Error::INVALID_ARG));
        let mut stream = audio.open(STEREO).unwrap();
        assert_eq!(stream.write(Pcm::S32(&[0, 0])), Err(Error::INVALID_ARG));
        assert_eq!(stream.write(Pcm::S16(&[0])), Err(Error::INVALID_ARG));
    }

    #[test]
    fn same_format_reconfigure_is_a_no_op() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        let mut stream = audio.open(STEREO).unwrap();
        shared.take_calls();
        stream.reconfigure(STEREO).unwrap();
        assert_eq!(shared.take_calls(), []);
        stream.reconfigure(Format { sample_rate: 44_100, ..STEREO }).unwrap();
        assert_eq!(
            shared.take_calls(),
            [Call::Drain, Call::Mute(true), Call::Open, Call::Gain(0.0)]
        );
    }

    #[test]
    fn dsp_volume_scales_samples_and_keeps_hw_at_0_db() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        audio.set_volume(100).unwrap();
        let mut stream = audio.open(STEREO).unwrap();
        stream.write(Pcm::S16(&vec![1000; FADE_SAMPLES])).unwrap();
        assert_eq!(*shared.take_samples().last().unwrap(), 1000);
        shared.take_calls();

        audio.set_mute(true).unwrap();
        assert_eq!(shared.take_calls(), []);
        stream.write(Pcm::S16(&vec![1000; 48_000])).unwrap();
        assert_eq!(*shared.take_samples().last().unwrap(), 0);
    }

    #[test]
    fn dsp_fades_in_from_the_first_sample_with_any_hardware() {
        for mute in [Mute::None, Mute::Step, Mute::Ramp] {
            let (driver, shared) = Stub::new(false, mute, false);
            let audio = audio_with(driver, DspMode::Manual);
            audio.set_volume(100).unwrap();
            let mut stream = audio.open(STEREO).unwrap();
            audio.set_volume(90).unwrap();
            audio.set_volume(100).unwrap();
            stream.write(Pcm::S16(&vec![1000; 9600])).unwrap();
            let samples = shared.take_samples();
            assert!(samples[0].abs() < 10);
            assert_eq!(*samples.last().unwrap(), 1000);
        }
    }

    #[test]
    fn close_without_a_write_never_unmutes() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        audio.open(STEREO).unwrap().close().unwrap();
        assert!(!shared.take_calls().contains(&Call::Mute(false)));
    }

    #[test]
    fn a_held_output_serves_the_next_stream_of_the_same_format() {
        let (mut driver, shared) = Stub::new(true, Mute::Ramp, false);
        driver.hold = Duration::from_secs(60);
        let audio = audio_with(driver, DspMode::Manual);
        let mut source = route(&audio);
        audio.open(STEREO).unwrap().close().unwrap();
        shared.take_calls();

        assert!(matches!(source.poll(), Next::After(_)));
        let mut stream = audio.open(STEREO).unwrap();
        stream.write(Pcm::S16(&[0, 0])).unwrap();
        let calls = shared.take_calls();
        assert!(!calls.contains(&Call::Open));
        assert!(!calls.contains(&Call::Close));
        assert!(calls.contains(&Call::Mute(false)));
        drop(stream);
        shared.take_calls();

        drop(audio.open(Format { sample_rate: 44_100, ..STEREO }).unwrap());
        assert!(shared.take_calls().contains(&Call::Open));
    }

    #[test]
    fn a_held_output_closes_when_the_time_is_up() {
        let (mut driver, shared) = Stub::new(true, Mute::Ramp, false);
        driver.hold = Duration::from_millis(20);
        let audio = audio_with(driver, DspMode::Manual);
        let mut source = route(&audio);
        audio.open(STEREO).unwrap().close().unwrap();
        shared.take_calls();

        assert!(matches!(source.poll(), Next::After(_)));
        assert!(!shared.take_calls().contains(&Call::Close));
        thread::sleep(Duration::from_millis(30));
        assert!(matches!(source.poll(), Next::Idle));
        assert!(shared.take_calls().contains(&Call::Close));

        drop(audio.open(STEREO).unwrap());
        assert!(shared.take_calls().contains(&Call::Open));
    }

    #[test]
    fn s32_stream_goes_through_the_dsp() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        audio.set_volume(100).unwrap();
        let mut stream = audio.open(Format { sample: SampleFormat::S32, ..STEREO }).unwrap();
        stream.write(Pcm::S32(&vec![0; FADE_SAMPLES])).unwrap();
        shared.bytes.lock().unwrap().clear();
        stream.write(Pcm::S32(&[1 << 20, -(1 << 20)])).unwrap();
        let bytes = core::mem::take(&mut *shared.bytes.lock().unwrap());
        let samples: Vec<i32> =
            bytes.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        assert_eq!(samples, [1 << 28, -(1 << 28)]);
    }

    #[test]
    fn no_dsp_moves_the_hardware() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Disable);
        assert!(audio.eq().is_none());
        assert_eq!(audio.set_eq_enabled(true), Err(Error::NOT_SUPPORTED));
        shared.take_calls();

        audio.set_volume(50).unwrap();
        assert_eq!(shared.take_calls(), [Call::Mute(true), Call::Gain(-20.0)]);

        let mut stream = audio.open(STEREO).unwrap();
        stream.write(Pcm::S16(&[1000, -1000])).unwrap();
        assert_eq!(shared.take_samples(), [1000, -1000]);
        shared.take_calls();

        audio.set_mute(true).unwrap();
        assert_eq!(shared.take_calls(), [Call::Mute(true), Call::Gain(-20.0)]);
        audio.set_mute(false).unwrap();
        audio.set_volume(150).unwrap();
        assert_eq!(shared.take_calls().last(), Some(&Call::Gain(0.0)));
    }

    #[test]
    fn no_dsp_without_hw_mute_mutes_on_the_gain() {
        let (driver, shared) = Stub::new(true, Mute::None, false);
        let audio = audio_with(driver, DspMode::Disable);
        audio.set_volume(50).unwrap();
        audio.set_mute(true).unwrap();
        assert_eq!(shared.take_calls().last(), Some(&Call::Gain(-96.0)));
    }

    #[test]
    fn no_dsp_without_hw_gain_has_no_volume_and_mutes_to_zeros() {
        let (driver, shared) = Stub::new(false, Mute::None, false);
        let audio = audio_with(driver, DspMode::Disable);
        assert_eq!(audio.set_volume(50), Err(Error::NOT_SUPPORTED));

        let mut stream = audio.open(STEREO).unwrap();
        stream.write(Pcm::S16(&[1000, -1000])).unwrap();
        assert_eq!(shared.take_samples(), [1000, -1000]);

        audio.set_mute(true).unwrap();
        stream.write(Pcm::S16(&[1000, -1000])).unwrap();
        assert_eq!(shared.take_samples(), [0, 0]);
    }

    #[test]
    fn speaker_modes_follow_the_headphone() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        let mut source = route(&audio);
        drop(audio.open(STEREO).unwrap());
        shared.take_calls();

        audio.set_speaker_mode(SpeakerMode::Auto).unwrap();
        shared.take_calls();
        source.poll();
        assert_eq!(shared.take_calls(), [Call::Amp(true)]);
        shared.headphone.store(true, Ordering::SeqCst);
        assert!(matches!(source.poll(), Next::After(ROUTE_POLL)));
        assert_eq!(shared.take_calls(), [Call::Amp(false)]);

        audio.set_speaker_mode(SpeakerMode::Off).unwrap();
        shared.headphone.store(false, Ordering::SeqCst);
        shared.take_calls();
        assert!(matches!(source.poll(), Next::Idle));
        assert_eq!(shared.take_calls(), [Call::Amp(false)]);
    }

    #[test]
    fn headphone_callback_fires_on_changes_only() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        let mut source = route(&audio);
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let sink = seen.clone();
        audio
            .set_headphone_callback(Some(Box::new(move |inserted| sink.lock().unwrap().push(inserted))))
            .unwrap();
        assert!(matches!(source.poll(), Next::After(ROUTE_POLL)));
        source.poll();
        shared.headphone.store(true, Ordering::SeqCst);
        source.poll();
        source.poll();
        shared.headphone.store(false, Ordering::SeqCst);
        source.poll();
        assert_eq!(*seen.lock().unwrap(), [true, false]);

        audio.set_headphone_callback(None).unwrap();
        assert!(matches!(source.poll(), Next::Idle));
    }

    #[test]
    fn auto_voices_each_route_and_keeps_the_eq_override() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, true);
        let audio = audio_with(driver, DspMode::Auto);
        let mut source = route(&audio);
        assert!(audio.tracks_route);
        assert!(audio.eq_enabled());
        source.poll();

        let mut matrix = [0.0; 4];
        audio.mixer().unwrap().matrix(0, &mut matrix).unwrap();
        assert_eq!(matrix, MATRIX_MONO);

        audio.set_eq_enabled(false).unwrap();
        let before = shared.profiles.load(Ordering::SeqCst);
        shared.headphone.store(true, Ordering::SeqCst);
        source.poll();
        assert_eq!(shared.profiles.load(Ordering::SeqCst), before + 1);
        assert!(!audio.eq_enabled());
        audio.mixer().unwrap().matrix(0, &mut matrix).unwrap();
        assert_eq!(matrix, MATRIX_STEREO);
    }

    #[test]
    fn mono_mix_applies_to_stereo_streams() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        audio.set_volume(100).unwrap();
        audio.mixer().unwrap().set_matrix(0, &MATRIX_MONO).unwrap();
        let mut stream = audio.open(STEREO).unwrap();
        stream.write(Pcm::S16(&vec![0; FADE_SAMPLES])).unwrap();
        shared.take_samples();
        stream.write(Pcm::S16(&[1000, 0])).unwrap();
        assert_eq!(shared.take_samples(), [500, 500]);
    }

    #[test]
    fn manual_starts_flat_and_is_not_tracked() {
        let (driver, shared) = Stub::new(true, Mute::Ramp, true);
        let audio = audio_with(driver, DspMode::Manual);
        assert!(!audio.tracks_route);
        assert_eq!(shared.profiles.load(Ordering::SeqCst), 0);
        audio.set_volume(100).unwrap();
        let mut stream = audio.open(STEREO).unwrap();
        stream.write(Pcm::S16(&vec![0; FADE_SAMPLES])).unwrap();
        shared.take_samples();
        stream.write(Pcm::S16(&[1000, -1000])).unwrap();
        assert_eq!(shared.take_samples(), [1000, -1000]);
    }

    #[test]
    fn queued_needs_a_running_stream() {
        let (driver, _shared) = Stub::new(true, Mute::Ramp, false);
        let audio = audio_with(driver, DspMode::Manual);
        let stream = audio.open(STEREO).unwrap();
        assert_eq!(stream.queued(), Ok(12));
    }
}
