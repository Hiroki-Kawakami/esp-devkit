# FLAC decoder (`audf_flac`)

## Scope

Every subframe type (constant, verbatim, fixed, LPC up to order 32), both
residual codings with escaped partitions, wasted bits, the three stereo
decorrelations, fixed and variable block sizes. 4 to 24 bits and 1 to 8
channels. The output format is fixed at create from STREAMINFO: a frame with
another rate, channel count or sample size is refused rather than followed,
and so is a block larger than STREAMINFO's maximum, which sizes the buffers.
32-bit FLAC is refused at create.

16 bits or fewer come out as S16 and more as S32, both shifted up to full
scale (24-bit for S32), so a 20-bit stream is not narrowed to 16 on its way to
an output that can take 24.

## Frames and errors

A FLAC frame carries no length; the container gives the boundaries (Matroska,
MP4) or the caller finds them (a native stream, by the sync word). The decoder
only reads as far as the frame it parses, so bytes after it are ignored, and
the codes tell a caller that is guessing a boundary what went wrong:

- `ESP_ERR_INVALID_SIZE`: the frame runs past `len`, so the boundary was too
  early, or the block is larger than STREAMINFO allows.
- `ESP_ERR_INVALID_CRC`: the header's CRC-8 or the frame's CRC-16 does not
  match; the PCM buffer is left untouched.
- `ESP_ERR_NOT_SUPPORTED`: a valid frame in another format.
- `ESP_ERR_INVALID_RESPONSE`: not a frame at all (no sync word, reserved
  values).

Nothing carries from one frame to the next, so `audf_decoder_reset()` has
nothing to do.

## Numbers

Samples are int32. The LPC sum is 32-bit when sample bits + coefficient
precision + bits of the order fit 32, and 64-bit otherwise (24-bit material at
high precision). Prediction and stereo arithmetic wraps as unsigned, so a
corrupt frame cannot hit signed overflow; its CRC fails anyway.

Memory is the decoder struct plus `max block × channels × 4` bytes, 36 KB for
a 4608-sample stereo stream, all from `alloc_caps`.

## Speed

C only, no PIE. On an ESP32-P4 at 360 MHz, decoding a clip held in PSRAM:
44.1 kHz/16-bit stereo at `-8` takes 5.3 % of a core, 96 kHz/24-bit stereo at
`-8` 15.7 %. Internal RAM for the samples makes no difference.

## Checking it

FLAC is lossless and the encoder stores the MD5 of the original samples in
STREAMINFO, so the host tool checks itself:

```sh
D=esp-devkit/libs/audio_framework
nix develop -c gcc -std=c11 -O2 -I $D/inc -I $D/src -I esp-devkit/idf_compat/include \
    $D/test/flac_dec_test.c $D/src/audf_flac.c $D/src/audf_alloc.c $D/src/audf_codec.c \
    -o flac_dec_test
./flac_dec_test in.flac [out.raw]
```

It prints `md5 ok` or `MISMATCH` (exit 1); `out.raw` is the samples at their
own width, signed little-endian, which for 16 and 24 bits is what
`ffmpeg -f s16le`/`s24le` writes. `--loops n` decodes again for timing.

Material:

- Synthetic: `test/make_flac_material.sh <dir>` encodes 8/16/24-bit, mono and
  stereo, 22.05-96 kHz with the reference encoder (`-0` to `-8`, order 32
  with partition orders up to 15, block sizes 192 to 16384, wasted bits) and
  with ffmpeg's (level 12, each stereo mode), from a temporary nix-shell.
- Conformance: `https://github.com/ietf-wg-cellar/flac-test-files`, `subset/`
  for streams every decoder must take, `uncommon/` and `faulty/` for the rest.

All 17 synthetic files and all 64 in `subset/` match their MD5. In
`uncommon/`, the 15-bit, 768 kHz, 65535-sample and partition-order-15 files
match; the ones that change rate, channels or bit depth mid-stream and the
32-bit one are refused as above.
