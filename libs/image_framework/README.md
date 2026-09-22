# image_framework

Streaming image decode, resize, recolor and encode. Decoders pull bytes from an
`imgf_stream_t` and hand back one row at a time, so an image never has to be
held whole; the resizer takes rows in and gives rows out on the same model.

On ESP32-P4 the hot loops have PIE (SIMD) kernels in `src/imgf_pie_p4.S`. Every
kernel has a C counterpart that writes exactly the same bytes, so the same
build runs on the host simulator and on a board that has no PIE, and
`firmware/imgf_bench` checks the two agree by comparing output checksums.

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

## Measuring

`firmware/imgf_bench` times decode, resize and the decode+resize pipeline on
the host and on a board, and prints a checksum per case. See its README.
