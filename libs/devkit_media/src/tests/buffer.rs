/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::ffi::CString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use super::{Memory, arena, bounded, c_path};
use crate::buffer::CHUNK_BYTES;
use crate::wav::WavDemuxer;
use crate::{ARENA_ALIGNMENT, Arena, Buffer, Demuxer, Error, OpenOptions, READ_ALIGNMENT};

const ARENA_BYTES: usize = 1024 * 1024;
const RING_BYTES: usize = 12 * CHUNK_BYTES;
const MAX_VIEW: usize = 4 * CHUNK_BYTES;

static NEXT_FILE: AtomicU32 = AtomicU32::new(0);

fn byte_at(offset: usize) -> u8 {
    (offset as u32).wrapping_mul(2654435761).to_be_bytes()[0] ^ (offset >> 16) as u8
}

struct File(PathBuf);

impl File {
    fn new(size: usize) -> Self {
        let name = format!("devkit_media_buffer_{}_{}", std::process::id(), NEXT_FILE.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        fs::write(&path, (0..size).map(byte_at).collect::<Vec<_>>()).unwrap();
        Self(path)
    }

    fn name(&self) -> CString {
        c_path(&self.0)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn buffer(file: &File) -> Buffer {
    let buffer = Buffer::open(&file.name(), arena(ARENA_BYTES)).unwrap();
    buffer.set_readahead(true);
    buffer
}

fn check(data: &[u8], offset: usize) {
    assert!(data.iter().enumerate().all(|(i, &b)| b == byte_at(offset + i)), "wrong bytes at {offset}");
}

// A seek would restart the window at another ring slot; reading keeps a file
// offset in the slot it maps to from the start.
fn advance_to(b: &Buffer, offset: usize) {
    let mut out = vec![0; CHUNK_BYTES];
    while (b.tell() as usize) < offset {
        let want = (offset - b.tell() as usize).min(CHUNK_BYTES);
        assert_eq!(b.read(&mut out[..want]), want);
    }
}

#[test]
fn reads_everything_with_and_without_readahead() {
    let file = File::new(3 * RING_BYTES + 123);
    let b = Buffer::open(&file.name(), arena(ARENA_BYTES)).unwrap();
    let mut out = vec![0; 100_000];
    assert_eq!(b.read(&mut out), 100_000);
    check(&out, 0);
    b.set_readahead(true);
    let mut at = 100_000;
    loop {
        let n = b.read(&mut out);
        if n == 0 {
            break;
        }
        check(&out[..n], at);
        at += n;
    }
    assert_eq!(at, 3 * RING_BYTES + 123);
    assert_eq!(b.tell() as usize, at);
}

#[test]
fn rejects_unusable_arenas() {
    let file = File::new(1000);
    assert!(matches!(Buffer::open(&file.name(), arena(CHUNK_BYTES)), Err(Error::Arena)));
    let mut memory = Memory::new(ARENA_BYTES);
    assert_eq!(memory.as_ptr() as usize % ARENA_ALIGNMENT, 0);
    let bytes = &mut memory[..];
    assert_eq!(bytes.len(), ARENA_BYTES);
    struct Skewed(Memory);
    impl core::ops::Deref for Skewed {
        type Target = [u8];
        fn deref(&self) -> &[u8] {
            &self.0[1..]
        }
    }
    impl core::ops::DerefMut for Skewed {
        fn deref_mut(&mut self) -> &mut [u8] {
            &mut self.0[1..]
        }
    }
    assert!(matches!(Buffer::open(&file.name(), Arena::new(Skewed(memory))), Err(Error::Arena)));
    assert!(matches!(Buffer::open(c"/nonexistent/file", arena(ARENA_BYTES)), Err(Error::Open)));
}

#[test]
fn a_view_holds_its_bytes_while_the_ring_moves() {
    let file = File::new(40 * CHUNK_BYTES);
    let b = buffer(&file);
    let held = b.view(1000).unwrap();
    let mut out = vec![0; CHUNK_BYTES];
    b.seek((2 * CHUNK_BYTES) as i64);
    for i in 2..6 {
        assert_eq!(b.read(&mut out), CHUNK_BYTES);
        check(&out, i * CHUNK_BYTES);
    }
    check(&held, 0);
}

#[test]
fn views_across_the_end_of_the_ring_use_the_bounce_area() {
    let file = File::new(60 * CHUNK_BYTES);
    let b = buffer(&file);
    let mut at = 0;
    let mut bounced = 0;
    while at + 3 * CHUNK_BYTES < 60 * CHUNK_BYTES {
        let size = CHUNK_BYTES + CHUNK_BYTES / 2 + 77;
        let view = b.view(size).unwrap();
        check(&view, at);
        let shared_ring = view.as_ptr() as usize % CHUNK_BYTES != (at % CHUNK_BYTES);
        bounced += usize::from(shared_ring);
        at += size;
    }
    assert!(bounced > 0);
}

#[test]
fn a_second_bounce_view_waits_for_the_first() {
    let file = File::new(40 * CHUNK_BYTES);
    bounded(20, move || {
        let b = std::sync::Arc::new(buffer(&file));
        advance_to(&b, RING_BYTES - 100);
        let first = b.view(1000).unwrap();
        check(&first, RING_BYTES - 100);
        assert_eq!(first.as_ptr() as usize % CHUNK_BYTES, 0);

        let other = b.clone();
        let waiter = thread::spawn(move || {
            advance_to(&other, 2 * RING_BYTES - 100);
            let view = other.view(1000);
            view.map(|v| {
                check(&v, 2 * RING_BYTES - 100);
                v.len()
            })
        });
        thread::sleep(Duration::from_millis(200));
        assert!(!waiter.is_finished());
        drop(first);
        assert_eq!(waiter.join().unwrap(), Ok(1000));
    });
}

#[test]
fn interrupt_fails_a_waiting_view_and_clears() {
    let file = File::new(40 * CHUNK_BYTES);
    bounded(20, move || {
        let b = std::sync::Arc::new(buffer(&file));
        let pin = b.view(100).unwrap();
        let interrupter = b.interrupter();

        // The ring cannot get past the pinned chunk, so this waits for ever.
        let other = b.clone();
        let waiter = thread::spawn(move || {
            other.seek((RING_BYTES - CHUNK_BYTES) as i64);
            let mut got = 0;
            let mut out = vec![0; CHUNK_BYTES];
            loop {
                match other.view(CHUNK_BYTES) {
                    Ok(view) => {
                        got += 1;
                        drop(view);
                    }
                    Err(e) => return (got, e, other.read(&mut out)),
                }
            }
        });
        thread::sleep(Duration::from_millis(200));
        assert!(!waiter.is_finished());
        interrupter.interrupt(true);
        let (_, error, _) = waiter.join().unwrap();
        assert_eq!(error, Error::Interrupted);
        interrupter.interrupt(false);
        drop(pin);
        let view = b.view_at(0, 100).unwrap();
        check(&view, 0);
    });
}

#[test]
fn direct_mode_reads_through_a_window_and_refuses_views() {
    let file = File::new(5 * CHUNK_BYTES);
    let b = Buffer::open(&file.name(), Arena::direct(Memory::new(ARENA_BYTES))).unwrap();
    b.set_readahead(true);
    assert!(b.view(100).is_err());
    let mut small = [0; 10];
    assert_eq!(b.read(&mut small), 10);
    check(&small, 0);
    let mut out = vec![0; 3 * CHUNK_BYTES];
    assert_eq!(b.read(&mut out), 3 * CHUNK_BYTES);
    check(&out, 10);
    b.seek(777);
    assert_eq!(b.read(&mut small), 10);
    check(&small, 777);
    b.seek((5 * CHUNK_BYTES - 4) as i64);
    assert_eq!(b.read(&mut small), 4);
    check(&small[..4], 5 * CHUNK_BYTES - 4);
}

#[test]
fn read_alloc_keeps_the_phase_of_the_file_offset() {
    let file = File::new(5 * CHUNK_BYTES);
    let b = buffer(&file);
    for offset in [0usize, 1, 63, 64, 1000, 70001] {
        let data = b.read_alloc(offset as i64, 5000).unwrap();
        check(&data, offset);
        assert_eq!(data.as_ptr() as usize % READ_ALIGNMENT, offset % READ_ALIGNMENT);
    }
    assert!(b.read_alloc((5 * CHUNK_BYTES - 10) as i64, 100).is_err());
}

#[test]
fn view_at_moves_the_cursor_only_when_it_must() {
    let file = File::new(40 * CHUNK_BYTES);
    let b = buffer(&file);
    b.seek(1000);
    check(&b.view_at(2000, 100).unwrap(), 2000);
    assert_eq!(b.tell(), 1000);
    check(&b.view_at(500, 100).unwrap(), 500);
    assert_eq!(b.tell(), 500);
    let far = 10 * CHUNK_BYTES;
    check(&b.view_at(far as i64, 100).unwrap(), far);
    assert_eq!(b.tell(), far as i64);
    assert!(b.view_at(-1, 10).is_err());
    assert!(b.view_at((40 * CHUNK_BYTES - 5) as i64, 10).is_err());
    assert!(b.view(MAX_VIEW + 1).is_err());
    assert!(b.view(0).is_err());
}

#[test]
fn views_drop_on_other_tasks_while_the_reader_runs() {
    let file = File::new(400 * CHUNK_BYTES);
    bounded(60, move || {
        let b = buffer(&file);
        let (tx, rx) = std::sync::mpsc::sync_channel::<(crate::View, usize)>(4);
        let consumer = thread::spawn(move || {
            let mut bytes = 0;
            while let Ok((view, offset)) = rx.recv() {
                check(&view, offset);
                bytes += view.len();
            }
            bytes
        });
        let (mut at, mut size, mut total) = (0, 1, 0);
        while at + size <= 400 * CHUNK_BYTES {
            tx.send((b.view(size).unwrap(), at)).unwrap();
            at += size;
            total += size;
            size = (size * 7 + 13) % (2 * CHUNK_BYTES) + 1;
        }
        drop(tx);
        assert_eq!(consumer.join().unwrap(), total);
    });
}

#[test]
fn a_view_outlives_the_buffer() {
    let file = File::new(20 * CHUNK_BYTES);
    let b = buffer(&file);
    let bytes = b.view(3 * CHUNK_BYTES).unwrap();
    drop(b);
    drop(file);
    check(&bytes, 0);
}

#[test]
fn a_packet_outlives_its_demuxer() {
    let path = std::env::temp_dir().join(format!("devkit_media_wav_{}", std::process::id()));
    let pcm: Vec<u8> = (0..400_000).map(byte_at).collect();
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x02\0\x44\xac\0\0\x10\xb1\x02\0\x04\0\x10\0data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    fs::write(&path, wav).unwrap();

    let mut demux = WavDemuxer::open(&c_path(&path), arena(ARENA_BYTES), &OpenOptions::default()).unwrap();
    let first = demux.read().unwrap().unwrap();
    let second = demux.read().unwrap().unwrap();
    drop(demux);
    let _ = fs::remove_file(&path);
    check(&first.data, 0);
    check(&second.data, first.data.len());
}

#[test]
fn an_interrupted_read_rewinds_the_demuxer() {
    let path = std::env::temp_dir().join(format!("devkit_media_wav2_{}", std::process::id()));
    let pcm: Vec<u8> = (0..4_000_000).map(byte_at).collect();
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x02\0\x44\xac\0\0\x10\xb1\x02\0\x04\0\x10\0data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    fs::write(&path, wav).unwrap();

    bounded(30, move || {
        let mut demux = WavDemuxer::open(&c_path(&path), arena(ARENA_BYTES), &OpenOptions::default()).unwrap();
        let interrupter = demux.interrupter();
        let held = demux.read().unwrap().unwrap();
        let mut queue = vec![held];
        // Pins everything until read-ahead parks and the next view has to wait.
        let stopper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            interrupter.interrupt(true);
            interrupter
        });
        let error = loop {
            match demux.read() {
                Ok(Some(packet)) => queue.push(packet),
                Ok(None) => panic!("ran off the end"),
                Err(e) => break e,
            }
        };
        assert_eq!(error, Error::Interrupted);
        let interrupter = stopper.join().unwrap();
        let read = queue.len();
        interrupter.interrupt(false);
        queue.clear();
        let packet = demux.read().unwrap().unwrap();
        let _ = fs::remove_file(&path);
        check(&packet.data, read * packet.data.len());
    });
}
