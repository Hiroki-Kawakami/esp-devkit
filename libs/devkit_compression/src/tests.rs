/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::{Error, Inflate, Lzw, LzwOptions, LzwWork, Result, Source, WINDOW_BYTES, Window};
use devkit_core::Zeroable;

const GIF8: LzwOptions = LzwOptions { literal_bits: 8, msb_first: false, early_change: false };
const GIF2: LzwOptions = LzwOptions { literal_bits: 2, msb_first: false, early_change: false };
const TIFF: LzwOptions = LzwOptions { literal_bits: 8, msb_first: true, early_change: true };

fn fixture(name: &str) -> Vec<u8> {
    let dir = std::env::var("DEVKIT_TEST_FIXTURES").expect("run test/run.sh, which generates the fixtures");
    std::fs::read(format!("{dir}/{name}")).unwrap()
}

fn zeroed<T: Zeroable>() -> Box<T> {
    unsafe { Box::new_zeroed().assume_init() }
}

fn noise(n: usize, mask: u8) -> Vec<u8> {
    let mut x = 12345u32;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1103515245).wrapping_add(12345) & 0x7FFF_FFFF;
            (x >> 16) as u8 & mask
        })
        .collect()
}

fn runs(n: usize) -> Vec<u8> {
    (0..n).map(|i| if (i / 300) % 2 != 0 { (i / 300) as u8 } else { 0 }).collect()
}

struct Chunks<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl Source for Chunks<'_> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(&self.data[..self.chunk.min(self.data.len())])
    }

    fn consume(&mut self, n: usize) {
        self.data = &self.data[n..];
    }
}

fn read_all(mut read: impl FnMut(&mut [u8]) -> Result<usize>, out_chunk: usize) -> Result<Vec<u8>> {
    let mut out = vec![0; 8192];
    let mut total = 0;
    while total < out.len() {
        let want = (out.len() - total).min(out_chunk);
        let got = read(&mut out[total..total + want])?;
        total += got;
        if got < want {
            break;
        }
    }
    out.truncate(total);
    Ok(out)
}

fn inflate_all(data: &[u8], in_chunk: usize, out_chunk: usize) -> Result<Vec<u8>> {
    let mut src = Chunks { data, chunk: in_chunk };
    let mut z = zeroed::<Inflate>();
    let mut window = zeroed::<Window>();
    z.start(&mut src)?;
    read_all(|out| z.read(&mut src, &mut window, out), out_chunk)
}

fn lzw_all(data: &[u8], options: &LzwOptions, in_chunk: usize, out_chunk: usize) -> Result<Vec<u8>> {
    let mut src = Chunks { data, chunk: in_chunk };
    let mut z = Lzw::new();
    let mut work = zeroed::<LzwWork>();
    z.start(options, &mut work)?;
    read_all(|out| z.read(&mut src, &mut work, out), out_chunk)
}

#[test]
fn inflate_decodes_each_block_type() {
    let (zlib_noise, zlib_runs, zlib_stored) =
        (fixture("zlib_noise.bin"), fixture("zlib_runs.bin"), fixture("zlib_stored.bin"));
    for (input, output) in [(4096, 8192), (1, 7), (13, 1)] {
        assert_eq!(inflate_all(&zlib_noise, input, output).unwrap(), noise(4096, 0xFF));
        assert_eq!(inflate_all(&zlib_runs, input, output).unwrap(), runs(5000));
        assert_eq!(inflate_all(&zlib_stored, input, output).unwrap(), noise(300, 0xFF));
    }
}

#[test]
fn inflate_reads_from_a_slice_and_restarts() {
    let data = fixture("zlib_runs.bin");
    let mut z = zeroed::<Inflate>();
    let mut window = zeroed::<Window>();
    for _ in 0..2 {
        let mut src = &data[..];
        z.start(&mut src).unwrap();
        let mut out = vec![0; 6000];
        assert_eq!(z.read(&mut src, &mut window, &mut out), Ok(5000));
        assert!(z.ended());
        assert_eq!(&out[..5000], runs(5000));
    }
    assert_eq!(WINDOW_BYTES, 32768);
}

#[test]
fn inflate_reads_a_bare_stream() {
    let data = fixture("zlib_runs.bin");
    let mut z = zeroed::<Inflate>();
    let mut window = zeroed::<Window>();
    let mut src = &data[2..];
    z.start_raw();
    let mut out = vec![0; 6000];
    assert_eq!(z.read(&mut src, &mut window, &mut out), Ok(5000));
    assert_eq!(&out[..5000], runs(5000));
}

#[test]
fn inflate_rejects_a_bad_header() {
    let mut bad = fixture("zlib_runs.bin");
    bad[1] ^= 1;
    assert_eq!(inflate_all(&bad, 4096, 4096), Err(Error::Corrupt));
    assert_eq!(inflate_all(&[], 4096, 4096), Err(Error::Truncated));
}

#[test]
fn inflate_stops_on_truncated_input() {
    let data = fixture("zlib_noise.bin");
    match inflate_all(&data[..data.len() / 2], 4096, 8192) {
        Ok(out) => assert!(out.len() < 4096),
        Err(e) => assert_eq!(e, Error::Truncated),
    }
}

#[test]
fn inflate_keeps_its_error() {
    let mut src: &[u8] = &[0x78, 0x9C, 0x07];
    let mut z = Inflate::new();
    let mut window = zeroed::<Window>();
    z.start(&mut src).unwrap();
    let mut out = [0; 16];
    assert_eq!(z.read(&mut src, &mut window, &mut out), Err(Error::Corrupt));
    assert_eq!(z.read(&mut src, &mut window, &mut out), Err(Error::Corrupt));
}

fn flushed() -> Vec<u8> {
    [runs(3000), noise(3000, 0xFF), runs(2000)].concat()
}

/// Has the first `avail` bytes of `data`, and more to come until all of them.
struct Trickle<'a> {
    data: &'a [u8],
    pos: usize,
    avail: usize,
}

impl Source for Trickle<'_> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(&self.data[self.pos..self.avail])
    }

    fn consume(&mut self, n: usize) {
        self.pos += n;
    }

    fn pending(&self) -> bool {
        self.avail < self.data.len()
    }
}

fn inflate_trickled(data: &[u8], gzip: bool, step: usize, out_chunk: usize) -> Result<Vec<u8>> {
    let mut src = Trickle { data, pos: 0, avail: step.min(data.len()) };
    let mut z = zeroed::<Inflate>();
    let mut window = zeroed::<Window>();
    if gzip { z.start_gzip(&mut src)? } else { z.start(&mut src)? };
    let mut out = Vec::new();
    let mut buf = vec![0; out_chunk];
    while !z.ended() {
        let got = z.read(&mut src, &mut window, &mut buf)?;
        out.extend_from_slice(&buf[..got]);
        if got < out_chunk && !z.ended() {
            assert!(src.pending(), "stopped short with all the input there");
            src.avail = (src.avail + step).min(data.len());
        }
    }
    Ok(out)
}

#[test]
fn inflate_goes_on_as_input_arrives() {
    let cases = [
        ("zlib_noise.bin", false, noise(4096, 0xFF)),
        ("zlib_runs.bin", false, runs(5000)),
        ("zlib_stored.bin", false, noise(300, 0xFF)),
        ("zlib_flush.bin", false, flushed()),
        ("gzip_runs.bin", true, runs(5000)),
        ("gzip_fields.bin", true, noise(4096, 0x0F)),
    ];
    for (name, gzip, want) in cases {
        let data = fixture(name);
        for (step, out_chunk) in [(1, 8192), (3, 1), (7, 13), (64, 4096), (1000, 7)] {
            assert_eq!(inflate_trickled(&data, gzip, step, out_chunk).unwrap(), want, "{name} by {step}");
        }
    }
}

#[test]
fn inflate_decodes_flushes_and_gzip() {
    for (input, output) in [(4096, 8192), (1, 7)] {
        assert_eq!(inflate_all(&fixture("zlib_flush.bin"), input, output).unwrap(), flushed());
    }
    for name in ["gzip_runs.bin", "gzip_fields.bin"] {
        let data = fixture(name);
        let mut src = &data[..];
        let mut z = zeroed::<Inflate>();
        let mut window = zeroed::<Window>();
        z.start_gzip(&mut src).unwrap();
        let mut out = vec![0; 6000];
        let got = z.read(&mut src, &mut window, &mut out).unwrap();
        assert!(z.ended(), "{name}");
        assert!(src.is_empty(), "{name}");
        assert_eq!(&out[..got], if name == "gzip_runs.bin" { runs(5000) } else { noise(4096, 0x0F) });
    }
}

fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut src = data;
    let mut z = zeroed::<Inflate>();
    let mut window = zeroed::<Window>();
    z.start_gzip(&mut src)?;
    read_all(|out| z.read(&mut src, &mut window, out), 8192)
}

#[test]
fn inflate_checks_the_checksums() {
    let zlib = fixture("zlib_runs.bin");
    let mut bad = zlib.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(inflate_all(&bad, 4096, 8192), Err(Error::Corrupt));
    assert_eq!(inflate_all(&zlib[..zlib.len() - 2], 4096, 8192), Err(Error::Truncated));

    let gz = fixture("gzip_fields.bin");
    let n = gz.len();
    for at in [n - 8, n - 1] {
        let mut bad = gz.clone();
        bad[at] ^= 1;
        assert_eq!(gunzip(&bad), Err(Error::Corrupt), "byte {at} from the end");
    }
    let mut bad = gz.clone();
    bad[10] ^= 1;
    assert_eq!(gunzip(&bad), Err(Error::Corrupt), "FHCRC");
    assert_eq!(gunzip(&gz[..n - 3]), Err(Error::Truncated));
    assert_eq!(gunzip(&gz[..5]), Err(Error::Truncated));
    assert_eq!(gunzip(&zlib), Err(Error::Corrupt));
}

#[test]
fn reading_before_start_fails() {
    let mut src: &[u8] = &[];
    let mut out = [0; 4];
    assert_eq!(Inflate::new().read(&mut src, &mut zeroed::<Window>(), &mut out), Err(Error::NotStarted));
    assert_eq!(Lzw::new().read(&mut src, &mut zeroed::<LzwWork>(), &mut out), Err(Error::NotStarted));
}

#[test]
fn a_source_error_is_reported() {
    struct Failing;
    impl Source for Failing {
        fn fill_buf(&mut self) -> Result<&[u8]> {
            Err(Error::Source)
        }
        fn consume(&mut self, _: usize) {}
    }
    assert_eq!(Inflate::new().start(&mut Failing), Err(Error::Source));
}

#[test]
fn lzw_gif() {
    let (noise_data, runs_data, narrow) =
        (fixture("gif_noise.bin"), fixture("gif_runs.bin"), fixture("gif_narrow.bin"));
    for (input, output) in [(4096, 8192), (1, 5), (255, 1)] {
        assert_eq!(lzw_all(&noise_data, &GIF8, input, output).unwrap(), noise(4096, 0xFF));
        assert_eq!(lzw_all(&runs_data, &GIF8, input, output).unwrap(), runs(5000));
        assert_eq!(lzw_all(&narrow, &GIF2, input, output).unwrap(), noise(1200, 0x03));
    }
}

#[test]
fn lzw_tiff() {
    let (noise_data, runs_data) = (fixture("tiff_noise.bin"), fixture("tiff_runs.bin"));
    for (input, output) in [(4096, 8192), (1, 3)] {
        assert_eq!(lzw_all(&noise_data, &TIFF, input, output).unwrap(), noise(4096, 0xFF));
        assert_eq!(lzw_all(&runs_data, &TIFF, input, output).unwrap(), runs(5000));
    }
}

#[test]
fn lzw_restart_reuses_the_work_area() {
    let data = fixture("tiff_runs.bin");
    let mut z = Lzw::new();
    let mut work = zeroed::<LzwWork>();
    for _ in 0..2 {
        let mut src = &data[..];
        z.start(&TIFF, &mut work).unwrap();
        let mut out = vec![0; 5000];
        assert_eq!(z.read(&mut src, &mut work, &mut out), Ok(5000));
        assert_eq!(out, runs(5000));
    }
}

#[test]
fn lzw_rejects_a_bad_code() {
    // Clear (4), then 7: a code past the first free entry.
    assert_eq!(lzw_all(&[0x3C, 0x00], &GIF2, 16, 16), Err(Error::Corrupt));
    let wide = LzwOptions { literal_bits: 9, ..GIF2 };
    assert_eq!(lzw_all(&[0x3C, 0x00], &wide, 1, 16), Err(Error::Corrupt));
}
