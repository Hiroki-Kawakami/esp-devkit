/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use devkit::Zeroable;

/// Upper bound on the channels of every module.
pub const MAX_CHANNELS: u8 = 8;

const S16_SHIFT: u32 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    S16,
    /// 24-bit PCM (full scale `1 << 23`) with 8 bits of headroom.
    S32,
}

impl Format {
    pub const fn bytes(self) -> usize {
        match self {
            Format::S16 => 2,
            Format::S32 => 4,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Pcm<'a> {
    S16(&'a [i16]),
    S32(&'a [i32]),
}

#[derive(Debug)]
pub enum PcmMut<'a> {
    S16(&'a mut [i16]),
    S32(&'a mut [i32]),
}

impl Pcm<'_> {
    pub fn format(&self) -> Format {
        match self {
            Pcm::S16(_) => Format::S16,
            Pcm::S32(_) => Format::S32,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Pcm::S16(s) => s.len(),
            Pcm::S32(s) => s.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl PcmMut<'_> {
    pub fn format(&self) -> Format {
        match self {
            PcmMut::S16(_) => Format::S16,
            PcmMut::S32(_) => Format::S32,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            PcmMut::S16(s) => s.len(),
            PcmMut::S32(s) => s.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A sample format between modules. Interleaved, one instance per format.
pub trait Sample: Copy + Default + PartialEq + Send + Sync + Zeroable + 'static {
    const FORMAT: Format;
    /// The working 32-bit value: S16 scaled up to the S32 full scale.
    fn load(self) -> i32;
    /// From a working value, rounded and saturated.
    fn store(v: i64) -> Self;
    fn pcm(data: &[Self]) -> Pcm<'_>;
    fn pcm_mut(data: &mut [Self]) -> PcmMut<'_>;
    fn from_pcm(pcm: Pcm<'_>) -> Option<&[Self]>;
    fn from_pcm_mut(pcm: PcmMut<'_>) -> Option<&mut [Self]>;
}

impl Sample for i16 {
    const FORMAT: Format = Format::S16;

    #[inline(always)]
    fn load(self) -> i32 {
        i32::from(self) * (1 << S16_SHIFT)
    }

    #[inline(always)]
    fn store(v: i64) -> Self {
        ((v + (1 << (S16_SHIFT - 1))) >> S16_SHIFT).clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
    }

    fn pcm(data: &[Self]) -> Pcm<'_> {
        Pcm::S16(data)
    }

    fn pcm_mut(data: &mut [Self]) -> PcmMut<'_> {
        PcmMut::S16(data)
    }

    fn from_pcm(pcm: Pcm<'_>) -> Option<&[Self]> {
        match pcm {
            Pcm::S16(s) => Some(s),
            Pcm::S32(_) => None,
        }
    }

    fn from_pcm_mut(pcm: PcmMut<'_>) -> Option<&mut [Self]> {
        match pcm {
            PcmMut::S16(s) => Some(s),
            PcmMut::S32(_) => None,
        }
    }
}

impl Sample for i32 {
    const FORMAT: Format = Format::S32;

    #[inline(always)]
    fn load(self) -> i32 {
        self
    }

    #[inline(always)]
    fn store(v: i64) -> Self {
        sat32(v)
    }

    fn pcm(data: &[Self]) -> Pcm<'_> {
        Pcm::S32(data)
    }

    fn pcm_mut(data: &mut [Self]) -> PcmMut<'_> {
        PcmMut::S32(data)
    }

    fn from_pcm(pcm: Pcm<'_>) -> Option<&[Self]> {
        match pcm {
            Pcm::S32(s) => Some(s),
            Pcm::S16(_) => None,
        }
    }

    fn from_pcm_mut(pcm: PcmMut<'_>) -> Option<&mut [Self]> {
        match pcm {
            PcmMut::S32(s) => Some(s),
            PcmMut::S16(_) => None,
        }
    }
}

#[inline(always)]
pub(crate) fn sat32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

pub(crate) fn check_channels(channels: u8) -> crate::Result<()> {
    if channels < 1 || channels > MAX_CHANNELS { Err(crate::Error::InvalidArg) } else { Ok(()) }
}
