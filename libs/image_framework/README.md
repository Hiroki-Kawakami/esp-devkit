# image_framework

Streaming image decode, resize, recolor and encode. Decoders pull bytes from an
`imgf_stream_t` and hand back one row at a time, so an image never has to be
held whole; the resizer takes rows in and gives rows out on the same model.

On ESP32-P4 the hot loops have PIE (SIMD) kernels in `src/imgf_pie_p4.S`. Every
kernel has a C counterpart that writes exactly the same bytes, so the same
build runs on the host simulator and on a board that has no PIE, and
`firmware/imgf_bench` checks the two agree by comparing output checksums.

## Decoders

JPEG, PNG, BMP, GIF and TIFF, each in `imgf_<format>d.h`; `imgf_sniff()` picks
one from the first bytes. Inflate and LZW come from `compression_framework`.

**`imgf_stream_t` has an optional `seek()`**, because two formats store their
rows out of order: a TIFF's IFD and strips may be anywhere in the file (libtiff
puts the IFD last), and an ordinary BMP is stored bottom-up. Reading the file
into memory instead would cost a whole encoded picture of PSRAM; seeking costs
one window refill per jump. A bottom-up BMP is read back in chunks of rows from
the end, so the reader is not refilled for every row. Without `seek()`, those
a TIFF and a bottom-up BMP return `IMGF_ERR_UNSUPPORTED`; everything else
still works.

The host tests compare BMP, GIF and TIFF against what Pillow decodes the same
file to (`test/gen_pillow_fixtures.py`). Pillow cannot write most of the BMP
variants, tiles or a GIF with fewer than 8 literal bits, so the generator writes
those itself and only uses Pillow to decode them.

## Using it on ESP32-P4

PIE's lazy register save writes 128-bit vectors to the calling task's stack,
and RTC RAM cannot take them. **A task that decodes or resizes must have a
stack that `MALLOC_CAP_SIMD` would satisfy** — create it with
`xTaskCreatePinnedToCoreWithCaps(..., MALLOC_CAP_SIMD)` rather than plain
`xTaskCreate`, which can land in RTC RAM once the other internal pools are
used up. The same applies to any buffer a caller hands in: the destination row
for `imgf_decoder_next_row()` and the destination image for the resizer are
written by PIE stores. Buffers the library allocates itself already ask for
`MALLOC_CAP_SIMD`.

Ask for `MALLOC_CAP_SIMD` **alone**, never OR-ed with `MALLOC_CAP_INTERNAL`:
the P4 heap matches capabilities one priority level at a time and lists the two
on different levels for `RETENT_RAM`, where most of the free internal memory
is, so the combination only matches the small `RAM` regions.

The component compiles itself at `-O2` unless the project asked for `-O0` or
`-O2` outright. At IDF's default the decode and resize loops run 1.4-1.6x
slower for 2 KB of flash.

## Orientation

`imgf_resize_opts_t::orientation` turns or mirrors the picture while it is
resized, so a picture stored on its side is shown upright without a second
full-size buffer. The target size is the oriented one. A row of the resized
picture may land as a column, so an oriented resizer only writes through
`imgf_resizer_pop_row_into()`, which takes the whole destination: the row is
packed as usual and then placed pixel by pixel.

## Measuring

`firmware/imgf_bench` times decode, resize and the decode+resize pipeline on
the host and on a board, and prints a checksum per case. See its README.
