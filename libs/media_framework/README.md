# media_framework

Media file containers: a read-ahead file buffer, tags and cover art, and
demuxers for AVI, WAV, MP3/MP2/ADTS AAC elementary streams, FLAC, Ogg Opus,
Matroska and MP4.
Demuxers hand out packets as views into the buffer; decoding them is the
caller's. Container-specific behaviour is in
[`docs/containers.md`](docs/containers.md).

| header | what |
|---|---|
| `medf_buffer.h` | read-ahead into a caller's arena, packets as views into it |
| `medf_tags.h` | the tag struct every demuxer fills, UTF-8 conversion, ID3v2/ID3v1 |
| `medf_riff.h` | the audio codec ids AVI and WAV share |
| `medf_avi.h`, `medf_wav.h` | RIFF demuxers on one chunk walk (`src/medf_riff.c`) |
| `medf_es.h` | MP3, MP2 and ADTS AAC elementary streams |
| `medf_flac.h` | native FLAC, and STREAMINFO for FLAC in other containers |
| `medf_alac.h` | the ALAC config inside a magic cookie, for MP4 and Matroska |
| `medf_ogg.h` | Ogg Opus |
| `medf_mkv.h` | Matroska |
| `medf_mp4.h` | ISO BMFF / QuickTime |

A format's header holds both directions: `medf_<format>_demux_*`, and
`medf_<format>_mux_*` for a format that gets a muxer.

## Buffer

FatFs only reads straight into the caller's buffer, via DMA, for whole sectors;
a partial sector at either end goes through its window and costs a copy.
Packets sit at whatever offset the muxer chose, so `medf_buffer` reads 64 KB
chunks at aligned offsets on its own task into the arena, and a demuxer hands
out views of that memory instead of copying packets out. File I/O is
`open()/read()`, outside the buffer's state lock.

- **The arena is the caller's**, aligned to `MEDF_ARENA_ALIGNMENT`. One quarter
  is a bounce area and the rest a ring of 64 KB chunks filled in ring order, so
  consecutive chunks are adjacent in memory. The bounce area is the largest
  view (`medf_arena_max_view()`).
- **A view pins the chunks it covers.** Read-ahead never writes into a pinned
  chunk; it waits. A view that would cross the ring's end is copied into the
  bounce area instead and pins that; a second such view waits until the first
  is released.
- **Nothing may hold a view longer than the reader takes to lap the ring.**
  Read-ahead parks on a pinned chunk and a view waits for read-ahead, so a pin
  the reader catches up with from behind stops both, with nothing in the log.
  Only `medf_buffer_interrupt()` and `medf_buffer_release_all()` clear it.
- **While interrupted, every read and view that has to wait fails.** Interrupt
  before waiting for a task that may be inside a read, and clear it as soon as
  that task has stopped: a demuxer's seek reads the file too, and fails while
  the flag is up wherever its target is outside the ring. An interrupted read
  rewinds the demuxer to the element it started on.
- **`medf_buffer_view_at()` leaves the cursor alone.** The cursor is a floor
  below which chunks are dropped; a view that would end more than half the ring
  past it moves the cursor to the view, which drops the window.
- **Demuxers parse headers with read-ahead off** and read the file directly.
  Only the sequential passes (indexes, playback) turn it on.
- **`direct` arenas are for header and tag reads**: no read-ahead, views fail,
  and reads come through a window in the bounce area that starts at 8 KB and
  doubles while reads stay sequential, so a header peek is cheap and the scan
  after it still gets 64 KB reads.
- **The read-ahead task** is `medf_readahead`, priority 3 on core 0, with its
  3 KB stack in PSRAM.

## Tags

`medf_tags_t` is filled by every demuxer. ID3 lives here rather than in the
elementary-stream demuxer because it turns up in more than one container: a
WAV or AVI `id3 ` chunk is the same bytes. Vorbis comments and the FLAC
`PICTURE` block are here for the same reason, since Ogg carries both.

- **Text is fixed-size UTF-8 inside the struct, not heap strings**, so the info
  structs are plain values a caller copies out of. The cover art is the only
  allocation, and `medf_tags_free()` at demux close is the only rule.
- **The first non-empty value of a field wins.** A parser walks from the better
  source to the worse one without tracking which it has seen: ID3v2, then the
  ID3v1 block at the end of the same MP3.
- **Only JPEG and PNG are kept, decided from the first bytes** rather than the
  declared type, up to `MEDF_COVER_MAX_BYTES`. Every Matroska attachment goes
  through the same check, so a font attachment is rejected without a rule about
  names.
- **`want_cover` false locates the picture without reading it**, which is
  nearly the whole cost of a tag on a file with cover art; `cover_at` then says
  where it is.
