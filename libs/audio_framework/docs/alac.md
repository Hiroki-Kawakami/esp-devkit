# ALAC decoder (`audf_alac`)

## Scope

The whole bitstream Apple's reference encoder and ffmpeg's write: single,
pair and LFE elements, adaptive Golomb residuals with runs of zeros, the
adaptive predictor in both modes, stereo unmixing, the low bytes stored apart
for 20 and 24 bits, verbatim frames, partial frames, and fill and data
elements, which are skipped. Coupling and program config elements are refused,
as the reference decoder does. 1 to 24 bits and 1 to 8 channels; 32-bit ALAC
and a compatible version above 0 are refused at create. C only, no PIE.

The output format is fixed at create from the ALACSpecificConfig (the 24 bytes
after the `alac` atom's version and flags): 16 bits or fewer come out as S16,
more as S32 shifted up to 24-bit scale, as with FLAC. `max_frames` is the
config's frame length, 352 from an AirPlay sender and 4096 in files.

Channels come in element order. For three or more that is ALAC's own layout
(centre first), which the caller reorders if it needs to.

## Frames and errors

The container gives the frame boundaries. Decoding stops once every channel
has been filled, so the closing end element and anything after it are not
read. ALAC has no checksum, so a corrupt frame is caught only when it breaks
the syntax:

- `ESP_ERR_INVALID_SIZE`: the frame runs past `len`, or holds more samples
  than the frame length.
- `ESP_ERR_NOT_SUPPORTED`: its elements carry more or fewer channels than the
  config.
- `ESP_ERR_INVALID_RESPONSE`: anything else that cannot be a frame (reserved
  bits set, a coupling element, a run of zeros past the end, a compressed
  element when the config's `kb` is 0, which ffmpeg writes when every frame
  is verbatim).

The predictor starts over in every element, so `audf_decoder_reset()` has
nothing to do.

## Numbers

Arithmetic follows the reference decoder, wrapping as unsigned where it would
overflow on corrupt input. Memory is the decoder struct plus three
`frame length × 4` byte buffers (two for mono), 48 KB for a 4096-sample
stream and 4 KB for AirPlay, all from `alloc_caps`.

## Checking it

ALAC is lossless, so the host tool compares against the source samples:

```sh
D=esp-devkit/libs/audio_framework
nix develop -c gcc -std=c11 -O2 -I $D/inc -I $D/src -I esp-devkit/idf_compat/include \
    $D/test/alac_dec_test.c $D/src/audf_alac.c $D/src/audf_alloc.c $D/src/audf_codec.c \
    -o alac_dec_test
./alac_dec_test --ref in.raw in.m4a [out.raw]
```

It reads the first ALAC track of an MP4 and prints `ref ok` or `MISMATCH`
(exit 1). `out.raw` is s16le for S16 and s24le for S32, which is what
`ffmpeg -f s16le`/`s24le` writes. `--loops n` decodes again for timing.

`test/make_alac_material.sh <dir>` writes each clip with its `.raw`: 16-bit
stereo and mono and 24-bit at 48 and 96 kHz, encoded by ffmpeg (default,
verbatim-only and order-30 settings) from a temporary nix-shell and by macOS's
`afconvert`, which adds the 20-bit mode and 352-frame packets. Both encoders
store the low byte apart at 24 bits; verbatim frames come from ffmpeg's
`-compression_level 0` and from `afconvert`'s 20-bit clip. All of them match.
Neither encoder writes the second predictor mode, so no material reaches that
path.
