/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;

use idf::fs::File;

use crate::mem::{Buffer, buffer};
use crate::{Error, Result};

/// Byte source of the decoders, read at block rate through a window of their own.
pub trait Stream {
    /// Bytes copied; 0 at the end of the stream.
    fn read(&mut self, dst: &mut [u8]) -> Result<usize>;

    /// Moves to `pos` bytes from the start. Formats that store their data out of
    /// order fail with `Unsupported` on a stream that cannot.
    fn seek(&mut self, _pos: usize) -> Result<()> {
        Err(Error::Unsupported)
    }

    fn can_seek(&self) -> bool {
        false
    }
}

impl<S: Stream + ?Sized> Stream for &mut S {
    fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        (**self).read(dst)
    }

    fn seek(&mut self, pos: usize) -> Result<()> {
        (**self).seek(pos)
    }

    fn can_seek(&self) -> bool {
        (**self).can_seek()
    }
}

/// An image in memory.
#[derive(Clone, Copy)]
pub struct SliceStream<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SliceStream<'a> {
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl Stream for SliceStream<'_> {
    fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        let rest = &self.data[self.pos..];
        let n = rest.len().min(dst.len());
        dst[..n].copy_from_slice(&rest[..n]);
        self.pos += n;
        Ok(n)
    }

    fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            return Err(Error::Io);
        }
        self.pos = pos;
        Ok(())
    }

    fn can_seek(&self) -> bool {
        true
    }
}

/// `length` bytes of a file from `offset`, or the rest of it when `length` is
/// `None`.
pub struct FileStream<'a> {
    file: &'a File,
    offset: u64,
    length: Option<usize>,
    pos: usize,
    started: bool,
}

impl<'a> FileStream<'a> {
    pub const fn new(file: &'a File, offset: u64, length: Option<usize>) -> Self {
        Self { file, offset, length, pos: 0, started: false }
    }
}

impl Stream for FileStream<'_> {
    fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        if !self.started {
            self.file.seek(self.offset).map_err(|_| Error::Io)?;
            self.started = true;
        }
        let n = match self.length {
            Some(length) => dst.len().min(length - self.pos),
            None => dst.len(),
        };
        if n == 0 {
            return Ok(0);
        }
        let got = self.file.read(&mut dst[..n]).map_err(|_| Error::Io)?;
        self.pos += got;
        Ok(got)
    }

    fn seek(&mut self, pos: usize) -> Result<()> {
        if self.length.is_some_and(|length| pos > length) {
            return Err(Error::Io);
        }
        self.file.seek(self.offset + pos as u64).map_err(|_| Error::Io)?;
        self.started = true;
        self.pos = pos;
        Ok(())
    }

    fn can_seek(&self) -> bool {
        true
    }
}

/// Byte sink of the encoders, written a few hundred bytes at a time.
pub trait Sink {
    fn write(&mut self, data: &[u8]) -> Result<()>;
}

impl<F: FnMut(&[u8]) -> Result<()>> Sink for F {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self(data)
    }
}

/// Writes into a buffer; running out of it is `OutOfMemory`.
pub struct SliceSink<'a> {
    data: &'a mut [u8],
    pos: usize,
}

impl<'a> SliceSink<'a> {
    pub const fn new(data: &'a mut [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub const fn written(&self) -> usize {
        self.pos
    }
}

impl Sink for SliceSink<'_> {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        let dst = self.data.get_mut(self.pos..self.pos + data.len()).ok_or(Error::OutOfMemory)?;
        dst.copy_from_slice(data);
        self.pos += data.len();
        Ok(())
    }
}

pub(crate) const WINDOW_BYTES: usize = 32768;

pub(crate) struct Reader<'a> {
    src: Box<dyn Stream + 'a>,
    pub(crate) buf: Buffer,
    pub(crate) len: usize,
    pub(crate) pos: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(src: Box<dyn Stream + 'a>) -> Result<Self> {
        Ok(Self { src, buf: buffer(WINDOW_BYTES, None)?, len: 0, pos: 0, end: 0 })
    }

    pub(crate) fn can_seek(&self) -> bool {
        self.src.can_seek()
    }

    // A failing source reads as the end of the stream: the decoders then report
    // the picture as cut short.
    #[cold]
    fn fill(&mut self) -> usize {
        let got = self.src.read(&mut self.buf).unwrap_or(0);
        self.end += got;
        self.pos = 0;
        self.len = got;
        got
    }

    pub(crate) fn read(&mut self, dst: &mut [u8]) -> usize {
        let mut total = 0;
        while total < dst.len() {
            if self.pos == self.len && self.fill() == 0 {
                break;
            }
            let k = (self.len - self.pos).min(dst.len() - total);
            dst[total..total + k].copy_from_slice(&self.buf[self.pos..self.pos + k]);
            self.pos += k;
            total += k;
        }
        total
    }

    pub(crate) fn read_exact(&mut self, dst: &mut [u8]) -> bool {
        self.read(dst) == dst.len()
    }

    #[inline]
    pub(crate) fn byte(&mut self) -> Option<u8> {
        if self.pos >= self.len && self.fill() == 0 {
            return None;
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Some(b)
    }

    pub(crate) fn tell(&self) -> usize {
        self.end - self.len + self.pos
    }

    pub(crate) fn seek(&mut self, pos: usize) -> bool {
        let start = self.end - self.len;
        if (start..=self.end).contains(&pos) {
            self.pos = pos - start;
            return true;
        }
        if self.src.can_seek() {
            if self.src.seek(pos).is_err() {
                return false;
            }
            self.len = 0;
            self.pos = 0;
            self.end = pos;
            return true;
        }
        if pos < self.end {
            return false;
        }
        self.skip(pos - self.tell())
    }

    pub(crate) fn skip(&mut self, mut n: usize) -> bool {
        let mut tmp = [0; 64];
        while n != 0 {
            let k = n.min(tmp.len());
            if !self.read_exact(&mut tmp[..k]) {
                return false;
            }
            n -= k;
        }
        true
    }
}
