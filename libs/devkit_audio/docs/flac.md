# FLAC decoder (`devkit_audio::flac`)

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

Nothing carries from one frame to the next, so `reset()` has
nothing to do.

## Numbers

Samples are int32. The LPC sum is 32-bit when sample bits + coefficient
precision + bits of the order fit 32, and 64-bit otherwise (24-bit material at
high precision). Prediction and stereo arithmetic wraps as unsigned, so a
corrupt frame cannot hit signed overflow; its CRC fails anyway.

Memory is the decoder struct plus `max block × channels × 4` bytes, 36 KB for
a 4608-sample stereo stream, all from `caps`.

## Checking it

`test/run.sh` decodes the material with `audio_framework` and holds this decoder to its output sample by sample.
