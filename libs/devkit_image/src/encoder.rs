/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::{Error, ImageInfo, Result, Stream};

pub trait Encoder {
    type Output;

    /// The size and the pixel format `push_row` expects.
    fn info(&self) -> ImageInfo;
    fn push_row(&mut self, row: &[u8]) -> Result<()>;
    fn finish(self) -> Result<Self::Output>;
}

/// Pushes every row of `src`, `stride` bytes apart (0 = tight), and finishes.
pub fn encode_buffer<E: Encoder>(mut encoder: E, src: &[u8], stride: usize) -> Result<E::Output> {
    let info = encoder.info();
    let row_bytes = info.row_bytes();
    let stride = if stride == 0 { row_bytes } else { stride };
    if stride < row_bytes {
        return Err(Error::InvalidArgument);
    }
    for y in 0..usize::from(info.height) {
        let row = src.get(y * stride..y * stride + row_bytes).ok_or(Error::InvalidArgument)?;
        encoder.push_row(row)?;
    }
    encoder.finish()
}

/// Pulls whole rows from `src` through `row`, which holds one input row, and
/// finishes. A short read is `Truncated`, a source error `Io`.
pub fn encode_stream<E: Encoder>(mut encoder: E, src: &mut dyn Stream, row: &mut [u8]) -> Result<E::Output> {
    let info = encoder.info();
    let row = row.get_mut(..info.row_bytes()).ok_or(Error::InvalidArgument)?;
    for _ in 0..info.height {
        let mut got = 0;
        while got < row.len() {
            match src.read(&mut row[got..]) {
                Ok(0) => return Err(Error::Truncated),
                Ok(n) => got += n,
                Err(_) => return Err(Error::Io),
            }
        }
        encoder.push_row(row)?;
    }
    encoder.finish()
}
