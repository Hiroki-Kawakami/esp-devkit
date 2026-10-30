# devkit_media

Media file containers: a read-ahead file buffer, tags and cover art, and
demuxers for AVI, WAV, MP3/MP2/ADTS AAC elementary streams, FLAC, Ogg Opus,
Matroska and MP4. Demuxers hand out packets as views into the buffer; decoding
them is the caller's. Container-specific behaviour is in
[`docs/containers.md`](docs/containers.md).

```rust
use devkit_media::{Arena, Demuxer, OpenOptions};

let memory: &'static mut [u8] = /* 64-byte aligned, in PSRAM */;
let mut demux = devkit_media::open(c"/sdcard/clip.mp4", Arena::new(memory), &OpenOptions { want_cover: true })?;
let info = demux.info();
while let Some(packet) = demux.read()? {
    send_to_decoder(packet);    // pts_us, keyframe, data: View
}
```

| module | what |
|---|---|
| crate root | `open`, the `Demuxer` trait, `Demux`, `Info`, `Packet`, the codec enums, `Buffer`, `View`, `Arena`, `Tags` |
| `avi`, `wav` | RIFF demuxers on one chunk walk |
| `es` | MP3, MP2 and ADTS AAC elementary streams |
| `flac` | native FLAC, and STREAMINFO for FLAC in other containers |
| `alac` | the ALAC config inside a magic cookie, for MP4 and Matroska |
| `ogg` | Ogg Opus |
| `mkv` | Matroska |
| `mp4` | ISO BMFF / QuickTime |

## Demuxers

`open()` picks a demuxer from the first bytes of the file and returns the
`Demux` enum; anything it does not recognize is tried as an elementary stream.
A demuxer's own type opens the same way (`Mp4Demuxer::open`) and adds what only
that container has: `AviDemuxer::seek_frame`, and `keyframe_at` and
`keyframe_before` on AVI, Matroska and MP4.

`Info` is the same for every container. `read()` returns `Ok(None)` at the end
and `Err` when it stopped short of it; `seek()` returns the time it landed on.
`set_want_audio(false)` leaves audio packets out.

A task that opens a file needs about 5 KB of stack; Ogg's `open` is the deepest.

## Buffer

FatFs only reads straight into the caller's buffer, via DMA, for whole sectors;
a partial sector at either end goes through its window and costs a copy.
Packets sit at whatever offset the muxer chose, so `Buffer` reads 64 KB chunks
at aligned offsets on its own task into the arena, and a demuxer hands out views
of that memory instead of copying packets out. File I/O is outside the buffer's
state lock.

- **The arena is the caller's**: anything `DerefMut<Target = [u8]> + Send +
  'static` aligned to `ARENA_ALIGNMENT`, such as a `&'static mut [u8]`, or a small
  wrapper over a `Box` or a `Lease` of aligned memory. One quarter is a bounce area and the rest a ring of 64 KB chunks filled in ring
  order, so consecutive chunks are adjacent in memory. The bounce area is the
  largest view (`Arena::max_view()`).
- **A `View` pins the chunks it covers** until it is dropped. Read-ahead never
  writes into a pinned chunk; it waits. A view that would cross the ring's end
  is copied into the bounce area instead and pins that; a second such view waits
  until the first is dropped.
- **Nothing may hold a view longer than the reader takes to lap the ring.**
  Read-ahead parks on a pinned chunk and a view waits for read-ahead, so a pin
  the reader catches up with from behind stops both, with nothing in the log.
  Only `Interrupter::interrupt()` gets out of it.
- **A `View` is `Send` and keeps the arena alive**, so a packet may go to another
  task and outlive its demuxer. The arena goes back when the last view is dropped.
- **To stop**, call `interrupt(true)` on an `Interrupter` from
  `Demuxer::interrupter()`, join the task that reads, drop every packet, and
  `interrupt(false)` as soon as that task has stopped. While it is set, every read
  and view that has to wait fails: a demuxer's seek reads the file too, and fails
  while the flag is up wherever its target is outside the ring. An interrupted read
  rewinds the demuxer to the element it started on.
- **`view_at()` leaves the cursor alone.** The cursor is a floor below which
  chunks are dropped; a view that would end more than half the ring past it moves
  the cursor to the view, which drops the window.
- **Demuxers parse headers with read-ahead off** and read the file directly.
  Only the sequential passes (indexes, playback) turn it on.
- **`Arena::direct` is for header and tag reads**: no read-ahead, views fail,
  and reads come through a window in the bounce area that starts at 8 KB and
  doubles while reads stay sequential, so a header peek is cheap and the scan after
  it still gets 64 KB reads.
- **The read-ahead task** is `media_readahead`, priority 3 on core 0, with its
  3 KB stack in PSRAM.
- **`tell()` takes no lock.** On another task than the one reading it can return
  a half-updated position.

## Tags

`Tags` is filled by every demuxer. ID3 lives apart from the elementary-stream
demuxer because it turns up in more than one container: a WAV or AVI `id3 ` chunk
is the same bytes. Vorbis comments and the FLAC `PICTURE` block are shared for the
same reason, since Ogg carries both.

- **Text is fixed-size UTF-8 inside the struct, not heap strings.** The cover art
  is the only other allocation.
- **The first non-empty value of a field wins.** A parser walks from the better
  source to the worse one without tracking which it has seen: ID3v2, then the
  ID3v1 block at the end of the same MP3.
- **Only JPEG and PNG are kept, decided from the first bytes** rather than the
  declared type, up to `COVER_MAX_BYTES`. Every Matroska attachment goes through the
  same check, so a font attachment is rejected without a rule about names.
- **`want_cover` false locates the picture without reading it**, which is nearly
  the whole cost of a tag on a file with cover art; `cover_at` then says where it is.
  `Demuxer::take_cover()` hands the picture over.

## Tests

`test/run.sh` runs the host tests inside the dev shell. `test/gen_material.py`
writes about 230 files with ffmpeg, lame, flac and opusenc from a temporary
nix-shell, adding by hand what those do not write (Matroska lacing and BlockGroups,
ID3 variants, Ogg packets across pages, `co64` and `stz2`) and the same files cut
short or with bytes stomped on. `test/dump_c.c` dumps what `media_framework` makes
of each file: info, tags, cover, every packet with a hash, seek landings and
keyframe queries, with several arena sizes. `src/tests/differential.rs` writes the
same text from this crate and compares. Without the material, in a plain
`rust_test`, only the `Buffer` tests run.

`firmware/rust_media_bench` compares the two on a board.
