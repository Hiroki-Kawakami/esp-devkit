# devkit_image

Streaming image decode, resize, recolor and encode. Decoders pull bytes from a
`Stream` and hand back one row at a time, so an image never has to be held whole;
the resizer takes rows in and gives rows out on the same model.

On ESP32-P4 the hot loops have PIE (SIMD) kernels in `src/imgf_pie_p4.S`. Every
kernel has a Rust counterpart that writes exactly the same bytes, so the same
crate runs on the host simulator and on a board that has no PIE, and
`firmware/rust_image_bench` checks that the two agree by comparing output
checksums.

## Decoders

JPEG, PNG, BMP, GIF, TIFF and WebP. `open` picks one from the first bytes of the
stream; `sniff` looks at a header the caller already has; `Decoder::open` and the
concrete types (`JpegDecoder`, ...) take the format as given. Inflate and LZW come
from `devkit_compression`.

```rust
let mut decoder = devkit_image::open(SliceStream::new(data), &DecodeOptions::default())?;
let info = decoder.info();
let mut row = vec![0; info.row_bytes()];
while decoder.next_row(&mut row)? {
    show(&row);
}
```

**`Stream::seek` is optional**, because three formats store their rows out of
order: a TIFF's directory and strips may be anywhere in the file (libtiff puts the
directory last), an ordinary BMP is stored bottom-up, and a lossy WebP keeps the
modes of every macroblock in one partition and their coefficients in up to eight
others, with its ALPH plane in a chunk of its own before them. Reading the file
into memory instead would cost a whole encoded picture of PSRAM; seeking costs one
window refill per jump. A bottom-up BMP is read back in chunks of rows from the
end, so the reader is not refilled for every row; a WebP reads each partition
through a window of its own. On a stream whose `can_seek` is `false`, a TIFF, a
bottom-up BMP and a lossy WebP return `Error::Unsupported`; everything else still
works. A source that fails reads as the end of the stream, so the decoder reports
`Truncated`.

**WebP output matches libwebp byte for byte**, including its fancy chroma upsampling
and its YUV to RGB arithmetic, which is what lets the host tests hold it to Pillow's
output exactly. A lossless WebP may copy from up to about a million pixels back, so
its decoder keeps a window of that many pixels, 4 bytes each, rather than the whole
picture; a smaller picture is held whole.

A progressive JPEG is read whole on the first `next_row`, which costs `w*h*2` bytes
of coefficients per component, allocated one block row at a time, so a fragmented
heap can still serve them. A truncated or damaged one keeps the scans that arrived.

## Resize

`Resizer` averages box areas on a downscale and is bilinear on an upscale, per axis,
and converts pixel formats in passing. `push_row` returns the rows it made ready,
which borrow the resizer, so the next push cannot overtake them:

```rust
let mut resizer = Resizer::new(info, &ResizeOptions { target_width: 640, target_height: 360, ..Default::default() })?;
for _ in 0..info.height {
    decoder.next_row(&mut src)?;
    let mut ready = resizer.push_row(&src)?;
    while ready.pop(&mut dst)? {
        show(&dst);
    }
}
let mut last = resizer.finish();
while last.pop(&mut dst)? {
    show(&dst);
}
```

`resize_decoder` and `resize_buffer` do this for a whole picture.
`resize_in_place` works on one buffer, for a picture that shrinks on every axis.
`DecodeOptions::target_width` lets JPEG do most of a large reduction while it
decodes.

### Orientation

`OrientedResizer` turns or mirrors the picture while it is resized, so a picture
stored on its side is shown upright without a second full-size buffer. The target
size is the oriented one. A row of the resized picture may land as a column, so its
`pop` takes the whole destination and a stride, and places the row pixel by pixel.

## Encoders

Both implement `Encoder`: `push_row`, then `finish`. `encode_buffer` and
`encode_stream` drive one over a whole picture.

- `JpegEncoder` is baseline, with the Annex K tables, integer only on its hot path.
  It writes to a `Sink` a few hundred bytes at a time, so neither side holds the
  whole image; `SliceSink` is a buffer, sized with `jpeg_buffer_upper_bound`, and a
  closure `FnMut(&[u8]) -> Result<()>` is a sink too. A sink's error is sticky.
- `RawEncoder` packs L8, I4, I1, RGB888 and RGB565 into a buffer at a fixed stride.

## Dither and recolor

`Ditherer` quantizes `Gray8` to N levels: Bayer 2/4/8 or error diffusion, integer
only. `process_row_in_place` and `dither_in_place` work on one buffer.

`Recolor` turns color into perceptual gray in two phases, so that the work straddles
a box downscale and stays photometrically correct: `to_intensity`, a `Gray8`
resizer, then `finalize`.

## Pipeline

`pipeline::run` runs a producer and a consumer on two tasks with a bounded ring of
rows between them, so that for instance decode and resize overlap with dither and
encode on two cores. The closures may borrow from the caller. An error on either
side aborts the other's wait, and the producer's error wins. `pipeline::start` is
the same for closures that own what they use, and returns a `Job` to poll.

## Using it on ESP32-P4

PIE's lazy register save writes 128-bit vectors to the calling task's stack, and RTC
RAM cannot take them. **A task that decodes or resizes must have a stack that
`Caps::SIMD` would satisfy**: create it with `thread::Builder::stack_caps(internal_caps())`
rather than a plain task, which can land in RTC RAM once the other internal pools
are used up. `pipeline` tasks do this by default. The same applies to any buffer a
caller hands in: the row given to `next_row` and the destination of the resizer are
written by PIE stores. Buffers the crate allocates itself already ask for it.

Ask for `Caps::SIMD` **alone**, never OR-ed with `Caps::INTERNAL`: the P4 heap
matches capabilities one priority level at a time and lists the two on different
levels for `RETENT_RAM`, where most of the free internal memory is, so the
combination only matches the small `RAM` regions.

## Testing and measuring

`test/run.sh` runs the host tests. The fixtures are not kept in git: the script runs
the generators inside `nix-shell`, which brings in libjpeg, cwebp, ffmpeg and Pillow,
into a temporary directory, so the encoders may differ from one run to the next. BMP,
GIF, TIFF and WebP are held to what Pillow decodes the same file to. Pillow cannot
write most of the BMP variants, tiles or a GIF with fewer than 8 literal bits, so the
generator writes those itself and only uses Pillow to decode them. The WebP files
come from cwebp, and from libvpx for a VP8 frame with several token partitions; a
lossless stream larger than the decoder's window is assembled by the generator and
checked by hash.

`firmware/rust_image_bench` times every case on the host and on a board, against
`libs/image_framework`.
