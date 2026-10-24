# MP3 decoder (`audf_mp3`)

`inc/audf_mp3.h`: MPEG-1, 2 and 2.5 audio Layer III (MP3) and MPEG-1 and 2
Layer II (MP2), plain C on every target. Written from ISO/IEC 11172-3 and
13818-3; the Huffman tables, the synthesis window and the Layer II allocation
tables are the standard's Annex B.

## Scope

- Mono, stereo, dual channel and joint stereo (mid/side and intensity, both
  the MPEG-1 and the MPEG-2 intensity scales). Long, short, start, stop and
  mixed blocks, every Huffman table, the bit reservoir.
- Not supported: Layer I, free format (bitrate index 0), the MPEG-2
  multichannel extension (its data is skipped, the stereo base decodes). CRCs
  are not checked.
- **The output is S16 with the channel count fixed at create.** A frame with
  the other count is doubled, or mixed down before the synthesis filterbank.
  The rate follows each frame (`audf_mp3_decoder_rate()`); a stream that
  changes it is the caller's to follow.
- One call takes one frame, from its header to the next one;
  `audf_mp3_parse_header()` gives the length. Cutting the stream into frames
  is the caller's job, since containers such as AVI cut packets anywhere.

## Frames and errors

- **Layer III frames borrow up to 511 bytes from the frames before them.** A
  frame whose data starts before what the decoder holds (the first frames
  after `audf_decoder_reset()`, or a stream cut in the middle) decodes as
  zero spectra: what comes out is the fading overlap of the frame before,
  which after a reset is silence. Its bytes still go into the reservoir, so
  decoding picks up at the next frame, and the timeline keeps its length.
- `ESP_ERR_INVALID_SIZE`: `len` is shorter than the header says.
  `ESP_ERR_NOT_SUPPORTED`: Layer I. `ESP_ERR_INVALID_RESPONSE`: no header,
  `big_values` above 288, or a switched window of block type 0. Corrupt
  Huffman data is not detected; the output stays bounded.
- **Intensity stereo follows the ISO reference decoder** where decoders
  differ: in short and mixed blocks the bound is found per window, the long
  part of a mixed block is intensity coded only when every window is empty
  above it, and the last band (which carries no scale factor) takes the band
  below only when that one is intensity coded. Mixed blocks use the long
  window for subbands 0 and 1 whenever the mixed flag is set, whatever the
  block type.

## Numbers

- Spectral and subband samples are PCM × 2^24 (`MP3_FRAC`). Requantized
  values are clamped at ±8: the `si_huff` conformance stream needs more than
  ±4.
- |q|^4/3 comes from a 1027-entry Q17 table; above 1023 it is interpolated
  at q/8 and multiplied by 16. The quarter-step gains 2^(k/4) are Q27: at Q14
  their 2^-16 error made loud passages differ from the reference by one LSB
  in two samples out of five.
- **Products take the high word of a 32 × 32 multiply** (`mp3_mulh`, one
  `mulh` on RV32) against Q31 coefficients: the alias butterflies, the IMDCT
  (an 18 × 18 DCT-IV, three 6 × 6 for short blocks), the synthesis matrixing
  (a 32-point DCT-II split into even and odd halves down to one point) and the
  window. A 64-bit multiply-accumulate is several instructions on RV32; with
  them a 128 kbps stereo stream took 18 % of a core instead of 10 %.
- **The IMDCT and the synthesis DCT run on block floating point**: the input
  is shifted up until its largest value sits just under 2^27 (2^26 for the
  DCT, whose butterflies add 32 values), and the result is shifted back.
  Without it the truncation of the high-word products, at 2^-23, raised the
  difference to the reference by a quarter. A constant added to correct the
  truncation instead adds a DC offset wherever the input is zero.
- The window keeps the standard's D[i] (multiples of 2^-16) × 2^30 and V × 4;
  the sum of |D| over one output's 16 taps is at most 2.73, so the sum stays
  in 32 bits.
- Why not a fast IMDCT through a DCT-II (y[m] + y[m−1] is the DCT-II of
  2 cos(π(2k+1)/72) x[k]): its recursion amplifies the rounding of the 32-bit
  products into errors of several LSB, and with 64-bit products it is slower
  than the direct form.

Memory is one 36.6 KB block from `alloc_caps` (two 8 KB synthesis buffers,
the reservoir, spectra, overlap and subband rows). The decoding stack stays
under 2.2 KB.

## Speed

Decoding a clip held in PSRAM on an ESP32-P4 rev 1 at 360 MHz, with the
decoder in PSRAM; cost is a percentage of one core. The clips are
`l3_44_128`, `l3_44_320` and `l2_44_256` from `test/make_mp3_material.sh`.

| clip | cost | stack |
|---|---|---|
| Layer III, 44.1 kHz stereo, 128 kbps | 10.2 % | 2140 B |
| Layer III, 44.1 kHz stereo, 320 kbps | 12.2 % | 2184 B |
| Layer II, 44.1 kHz stereo, 256 kbps | 7.1 % | 1896 B |

At 320 kbps the synthesis filterbank takes about half of the time, the IMDCT
and overlap 30 %, Huffman decoding and requantization 8 % each. The decoder in
internal RAM is no faster.

## Checking it

```sh
D=esp-devkit/libs/audio_framework
nix develop -c gcc -std=c11 -O2 -I $D/inc -I $D/src -I esp-devkit/idf_compat/include \
    $D/test/mp3_dec_test.c $D/src/audf_mp3*.c $D/src/audf_alloc.c $D/src/audf_codec.c \
    -lm -o mp3_dec_test
./mp3_dec_test [--channels n] [--native] [--ref ref.raw] in.mp3 [out.raw]
```

The tool skips an ID3v2 tag and a Xing/Info frame, cuts the rest into frames
by their headers and writes s16le. `--ref` compares against s16le and prints
the ISO 11172-4 verdict: full accuracy is an RMS difference below 1/√12 LSB
with no sample more than 2 LSB off (exit 1 otherwise). `--channels` fixes the
output channel count; `--native` writes mono frames as one channel, which is
the layout of the `he_mode` reference.

Material:

- Synthetic: `test/make_mp3_material.sh <dir>` encodes Layer III at every
  sample rate (CBR, VBR, mono, plain and joint stereo, forced mid/side, no
  reservoir) with libmp3lame and lame, and Layer II with ffmpeg and twolame
  (joint stereo, dual channel, CRC), from a temporary nix-shell. The reference
  is mpg123: ffmpeg's s16 output is half a step low and its MP2 decoder about
  0.4 LSB RMS off mpg123's float output, which ours matches to the rounding.
- Conformance: the ISO 11172-4 Layer III streams with their reference output,
  in FFmpeg's FATE suite
  (`rsync://fate-suite.ffmpeg.org/fate-suite/mp3-conformance/`). LAME writes
  neither intensity stereo nor mixed blocks; these streams do.

Every synthetic file is full accuracy against mpg123 (RMS 0.07 for Layer II,
0.12-0.16 for Layer III), and `compl`, `he_32khz`, `he_44khz`, `he_48khz`,
`hecommon`, `si`, `si_block` and `si_huff` against their references (RMS
0.07-0.14). `he_free` is free format. `sin1k0db`'s reference leaves out the
two frames whose reservoir lies before the stream; from there it is full
accuracy (RMS 0.18). `he_mode` matches except in frames 91-109, mixed blocks
with intensity stereo, where the reference is up to 11 LSB off both this
decoder and mpg123, which agree.
