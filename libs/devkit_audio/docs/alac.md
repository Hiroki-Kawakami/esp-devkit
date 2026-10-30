# ALAC decoder (`devkit_audio::alac`)

## Scope

The whole bitstream Apple's reference encoder and ffmpeg's write: single,
pair and LFE elements, adaptive Golomb residuals with runs of zeros, the
adaptive predictor in both modes, stereo unmixing, the low bytes stored apart
for 20 and 24 bits, verbatim frames, partial frames, and fill and data
elements, which are skipped. Coupling and program config elements are refused,
as the reference decoder does. 1 to 24 bits and 1 to 8 channels; 32-bit ALAC
and a compatible version above 0 are refused at create. No PIE.

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

The predictor starts over in every element, so `reset()` has
nothing to do.

## Numbers

Arithmetic follows the reference decoder, wrapping as unsigned where it would
overflow on corrupt input. Memory is the decoder struct plus three
`frame length × 4` byte buffers (two for mono), 48 KB for a 4096-sample
stream and 4 KB for AirPlay, all from `caps`.

## Checking it

`test/run.sh` decodes the material with `audio_framework` and holds this decoder to its output sample by sample.
