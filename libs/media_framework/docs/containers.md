# Containers

What each demuxer does with the files it meets. The buffer and tag rules they
all follow are in the [README](../README.md).

## AVI

- **Streams are matched by `strl` order, not by fixed chunk ids.** A file that
  declares audio first still plays.
- **`idx1` offsets are ambiguous.** Some muxers make them relative to `movi`,
  others make them absolute. The demuxer tries both against the first video
  chunk. If neither matches, seeking is disabled and reading still works.
- **`suggested_buffer_size` is ignored.** Muxers often write 0 there.
  `max_frame_bytes` comes from the largest chunk listed in `idx1`.
- **H.264 and MPEG-2 keyframes come from `idx1` (`AVIIF_KEYFRAME`).** The
  index keeps only keyframes as seek points, so `medf_avi_demux_seek()` lands
  on the last keyframe at or before the target and reports that frame. Without
  `idx1` a packet is a keyframe when its first slice NAL is IDR (type 5) or its
  picture header says I, and only a seek to frame 0 works. MJPEG ignores the
  flags.
- **Zero-size video chunks are frames.** ffmpeg writes them to keep the frame
  clock (its H.264 AVIs have one right after the first frame), so frame numbers
  count them, matching ffprobe's dts, and every keyframe there is one frame
  later than in the same encode muxed to MKV.
- **ffmpeg's H.264 AVIs have no extradata** (libx264 only emits it with
  `-flags +global_header`, and then as Annex-B). ffmpeg refuses to mux
  length-prefixed H.264 into AVI, so the avcC branch is untested on real files.
  `strf` extradata is cut to `biSize - 40`; the chunk carries a pad byte.
- **The API is frame-numbered**, since AVI has no timestamps; every other
  demuxer is in µs.

## WAV

WAV is the same envelope as AVI, not the same demuxer. The RIFF chunk walk, the
`fmt ` WAVEFORMATEX and the format-tag table are the same bytes, so they live in
`medf_riff.c` and both demuxers call them; everything else differs (AVI is
indexed chunk streams on a frame timeline, WAV is one `data` range with no
packet boundaries at all).

- **Packets are cut by the demuxer, not by the file.** A packet is about 32 KB
  rounded down to a whole PCM frame, or to a whole IMA ADPCM block, so a
  decoder that splits by `block_align` gets whole blocks. pts and seek are byte
  arithmetic and therefore exact.
- **A `data` size of 0, or one past the end of the file, means the file is what
  a streaming writer left behind**, so it is clamped to the file size.
- **`WAVE_FORMAT_EXTENSIBLE` resolves through the first two bytes of the
  subformat GUID**, which is the tag the file would have used without it.
- **The walk does not stop at `data`**, because `LIST INFO` is written after
  the audio as often as before it, and stepping over a chunk is a seek.

## MP3 and ADTS AAC

Both are self-delimiting frames with no container: the ID3/APE trimming, the
resync and the byte-to-time seek are the same code, and only the frame header
parser differs. The codec is decided at open by trying MP3 and then AAC on the
first frames.

- **A sync word only counts when three frames chain into each other.**
  Compressed payload is full of `FF` bytes, so a single header match finds
  false positives, in the file's first bytes as much as after a seek.
- **ID3v2 is skipped from the size in its header** (plus the footer when the
  flag is set), and ID3v1 and an APEv2 footer are trimmed off the end, so
  neither is handed out as audio.
- **The duration is exact only with Xing/Info or VBRI**, which carry the frame
  count. Without one it is the data size scaled by an average of the first 64
  frames. One frame is not enough: an encoder's first frames are much smaller
  than its steady state, which sized a 6 s AAC clip at 7 s.
- **The Xing frame is not handed out.** It is a valid, silent frame; it is
  dropped, and the byte-to-time mapping starts after it.
- **Seeking is an estimate**: the byte position comes from the time ratio
  (through the Xing TOC when there is one), and the time it reports back is
  that byte position converted again, so a position derived from it matches
  what plays. Only CBR lands where it says.
- **The resync scan buffer is 4 KB of the demuxer's own**, allocated at open.

## FLAC

`medf_flac_demux` reads the metadata blocks (STREAMINFO, SEEKTABLE, Vorbis
comment, PICTURE; an ID3v2 tag in front and an ID3v1 block at the end are
read and skipped) and hands out one frame per packet.

- **A frame carries no length, so it ends where the next frame's header is
  found.** The header is only trusted when its CRC-8 holds, its fields agree
  with STREAMINFO, and it starts at the sample the previous frame ends at.
  Sync words turn up in compressed data, and the sample number is what rules
  them out. The packet is a view of a window from the frame on, cut at that
  header; the window starts at STREAMINFO's maximum frame size and doubles
  while the header is not in it, because that maximum can be wrong.
- **The sample number decides where a frame is.** A fixed-size stream numbers
  its frames and a variable one its samples, but old encoders (Flake 0.11)
  wrote sample numbers under the fixed-size flag; the second frame's number
  tells the two apart at open.
- **Seeking is a bisection over bytes**, started from the SEEKTABLE points
  around the target when there are any: guess a byte offset, find the next
  trusted header there, and keep the half that holds the target, then step
  frame by frame. It lands on the start of the frame holding the target and
  reports that frame's time. A search never looks further for a header than a
  frame stored verbatim could be long.
- **The length is STREAMINFO's sample count**, or the last frame's end when
  that is 0.

## Matroska

`medf_mkv_demux` walks the stream flat: `Cluster` and `BlockGroup` are entered,
and every other element is skipped by its size. Top-level IDs never collide
with cluster children, so an unknown-size `Cluster` ends where the next
top-level element starts without tracking the hierarchy.

- **Tracks.** The first video track must be `V_MJPEG`, `V_MPEG4/ISO/AVC` or
  `V_MPEG2` (their `CodecPrivate`, avcC or a sequence header, is handed up
  raw). Audio is `A_PCM/INT/LIT`, `A_MPEG/L3`, `A_AAC*`, `A_OPUS`, `A_FLAC`,
  or `A_MS/ACM` whose WAVEFORMATEX is IMA ADPCM (`0x0011`); anything else is
  ignored as unsupported. `CodecPrivate` is handed up as-is, except for
  `A_MS/ACM`, where the rate, channels and block align come from the
  WAVEFORMATEX and only its extra bytes are passed on, and `A_FLAC`, where
  only the STREAMINFO body is, and the format comes from it. A track with
  `ContentEncodings` (header stripping, compression) is unsupported.
- **Lacing.** A laced audio block is split and each frame is a packet of its
  own, because raw AAC and Opus frames carry no length and a decoder cannot
  find the boundaries in concatenated data. mkvmerge laces audio by default;
  ffmpeg never does. Every frame takes its own `medf_buffer_view()` instead of
  one view over the whole block: the pending frames then hold no pin, so
  `medf_buffer_release_all()` cannot pull memory from under them, and an
  interrupted view rewinds to that frame. A block whose lace sizes do not add
  up is skipped, and so is a laced video block.
- **Keyframes.** A `SimpleBlock` has a keyframe flag; a `Block` has none and is
  a keyframe when its `BlockGroup` has no `ReferenceBlock`. The walk is flat,
  so for a video `Block` the rest of its group is read ahead and the reader
  steps back to the payload. That backward seek can throw away the read-ahead
  window, which is acceptable because muxers use `BlockGroup` for video only on
  odd frames (ffmpeg: a duration that differs from the default).
- **Timing.** `DefaultDuration` gives the frame interval; without it, the first
  two video blocks are measured. `Info/Duration` gives the length; without it,
  the last cue plus one interval. Timestamps are rounded to `TimestampScale`
  (1 ms from ffmpeg), so a frame at `k × interval` can be a fraction of a
  millisecond early.
- **Seeking needs `Cues`**, found through `SeekHead` or before the first
  cluster. Without them only a seek to 0 works. A seek jumps to the last cue at
  or before the target. The index is decimated past 60000 entries.
  - MJPEG lands on the block on screen at the target, which starts up to 2 s
    before it when unchanged frames were merged: from the cue it reads block
    headers up to the first video block past the target and goes back to the
    last one before it. Audio blocks before the target are skipped. ffmpeg
    starts a cluster for every MJPEG frame, so the index keeps one entry per
    cluster.
  - H.264 must start at a keyframe, so nothing near the target is skipped:
    blocks before the cue's own time are dropped (a cluster can hold more than
    one cued keyframe, so cues are not merged per cluster) and so are video
    blocks until the first keyframe, in case a cue points at a cluster that
    does not start with one. The first video packet's pts is where the seek
    landed. ffmpeg cues only video keyframes.
- **`Tags` and `Attachments` may sit after the clusters**, where the header
  walk stops. Their `SeekHead` entries are recorded like the Cues' one and read
  before the walk returns; a file with neither loses nothing.
- **Tag target levels are ignored.** ffmpeg writes a file-level TITLE at the
  album level (50), so honouring the level would turn every song title into an
  album name.

## MP4

`medf_mp4_demux` reads `moov` whole into PSRAM (up to 16 MB) and uses the
sample tables in place. They are never expanded per sample, since a two-hour
file has over half a million samples. Each track has a cursor (sample, chunk,
current `stsc`/`stts`/`ctts` run, next sync sample) that steps in constant
time; a seek rebuilds it by walking the runs, which are few.

- **Finding `moov`.** Top-level boxes are skipped by size, so a `moov` after
  `mdat` (no faststart) works at the cost of one seek on open. Fragmented files
  (`moof`) are rejected.
- **Tracks.** The best `vide` and `soun` track wins: a supported codec first,
  then the `tkhd` enabled flag. Video is `avc1`/`avc3` (`avcC`), `jpeg`/`mjpa`,
  `m2v1`, or `mp4v` whose esds says MJPEG (object type 0x6C, which is what
  ffmpeg writes for MJPEG in `.mp4`) or MPEG-2 video (0x60-0x65). Audio is
  `mp4a` with an AAC or MP3 object type (the esds may sit inside a QuickTime
  `wave`), `.mp3`, `Opus`, `fLaC`, or 16-bit `sowt`. QuickTime sound
  descriptions v1/v2 are longer, and their extra header is skipped.
- **`dfLa` is cut to its STREAMINFO body**, which is what a FLAC decoder takes,
  and the rate comes from there: the sample entry's 16.16 rate cannot hold
  more than 65535 Hz.
- **A file needs one usable track.** With no video track, seek moves the audio
  cursor and reports its own pts, `stss` and the keyframe index stay
  video-only, and the duration falls back to the audio `mdhd` when `mvhd` has
  none. A video track that is present but unsupported still fails the open, so
  a broken video file does not quietly read as audio.
- **`dOps` is rewritten as an OpusHead**, because that is what Opus decoders
  take (big-endian fields become little-endian; the mapping table is the same).
- **A `sowt` sample is one PCM frame**, and `stsz` often says 1 byte, so the
  size comes from the channel count and a chunk is handed out in packets of up
  to 32 KB rather than one packet per frame.
- **Timing.** pts is dts + ctts − the first edit's `media_time`, plus any
  leading empty edits. ffmpeg's AAC edit skips 1024 priming samples, so the
  first audio packet has a negative pts. Later edits are ignored with a
  warning. The frame interval is the most common `stts` delta, or for MJPEG the
  shortest one: a slideshow stores each still once, and its long delta can be
  the most common. The length is `mvhd`'s, or the video `mdhd`'s without it.
- **Rotation** is `-atan2(b, a)` of the `tkhd` matrix, as in ffmpeg's
  `av_display_rotation_get`, which is the same counter-clockwise angle as the
  Matroska roll.
- **Seeking** uses `stss`. H.264 lands on the last sync sample at or before the
  target and reports its pts; MJPEG lands on the sample on screen at the target
  and reports the target. Audio starts at its first sample at or after where
  the video landed.

**Packets come out in timestamp order, not file order.** Cameras, phones and
Apple tools interleave in chunks of 0.5-1 s (ffmpeg interleaves much finer).
Read in file order, a reader with a few packets of queue per track would fill
one track's queue a second ahead while the other track starves. So the demuxer
returns whichever track's next sample is earlier and reads backwards in the
file when needed.

- **The cursor follows the lower track.** Reads go through
  `medf_buffer_view_at()`, and the demuxer moves the cursor forward to the
  lower of the two tracks' next offsets, so the window keeps everything between
  them and a backward read is a cache hit.
- **Files that are not interleaved at all still read, slowly.** When a view
  would end more than half the ring past the cursor, the window is dropped, and
  the next read of the other track drops it again, so every switch re-reads
  from the card. Such files are rare, so nothing smarter is done; the demuxer
  logs once when the tracks are that far apart.
