# MP3 decoder (`devkit_audio::mp3`)

`src/mp3/`: MPEG-1, 2 and 2.5 audio Layer III (MP3) and MPEG-1 and 2
Layer II (MP2), with PIE kernels on the ESP32-P4 and a scalar build everywhere
else; both produce the same samples. Written from ISO/IEC 11172-3 and
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
  The rate follows each frame (`Mp3Decoder::rate()`); a stream that
  changes it is the caller's to follow.
- One call takes one frame, from its header to the next one;
  `mp3::parse_header()` gives the length. Cutting the stream into frames
  is the caller's job, since containers such as AVI cut packets anywhere.

## Frames and errors

- **Layer III frames borrow up to 511 bytes from the frames before them.** A
  frame whose data starts before what the decoder holds (the first frames
  after `reset()`, or a stream cut in the middle) decodes as
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
- **Long products split both sides into 16-bit halves**, x = xh · 2^16 + xl
  and c = ch · 2^16 + cl with xl, cl signed (`split`), because the PIE
  multiplies only 16 × 16. `xl · cl` is dropped.
  - The IMDCT (18 × 18 DCT-IV) and the synthesis matrixing (a 32-point DCT-II
    folded once into a 16-point DCT-II and a 16-point DCT-IV) are dot
    products: `sat(Σ xh·ch) + sat((Σ xh·cl + xl·ch) >> 16)`, about
    `Σ (x · c) >> 32` against Q31 coefficients, rounded down once per sum.
    The first DCT-II row is all ones and is a plain sum, since 1.0 has no Q31
    split.
  - The window is per output: `Σ vh·c + ((vl·c) >> 16)` over 22 rows of
    16-bit window parts. The standard's D[i] are multiples of 2^-16 up to
    1.144, so the four central tap rows take two or three parts, and the
    window is exact up to the floor of each low product (`SYNTH_FLOOR_BIAS`
    adds half of them back). The signs of V are folded into the parts, so the
    64 values a block writes are the matrixing output without negation, kept
    twice (at `off` and `off + 1024`) so that no tap wraps.
  - The alias butterflies, the short-block IMDCT and the window of the IMDCT
    output stay scalar, as the high word of a 32 × 32 multiply (`mulh`,
    one `mulh` on RV32). A 64-bit multiply-accumulate is several instructions
    there.
- **The IMDCT and the synthesis DCT run on block floating point**: the input
  is shifted up until its largest value sits just under 2^27 (2^25 for the
  DCT, whose butterflies add 32 values), and the result is shifted back.
  Without it, rounding at 2^-23 raised the difference to the reference by a
  quarter. A constant added to correct the rounding instead adds a DC offset
  wherever the input is zero.
- Synthesis input is PCM × 2^23 and V is × 2^24, clamped at ±32 so that the
  window sum (Σ|D| ≤ 2.73 over one output's taps) stays in 32 bits. IMDCT
  outputs and overlaps are clamped at ±32 for the same reason.
- Why not a fast IMDCT through a DCT-II (y[m] + y[m−1] is the DCT-II of
  2 cos(π(2k+1)/72) x[k]): its recursion amplifies the rounding of the 32-bit
  products into errors of several LSB, and with 64-bit products it is slower
  than the direct form.

Memory is one 36.6 KB block from `caps` (two 8 KB synthesis buffers,
the reservoir, spectra, overlap and subband rows), 16-byte aligned. The
decoding stack stays under 2 KB.

## PIE kernels

`src/mp3/kernels_p4.S` on the ESP32-P4; elsewhere the scalar versions in
`src/mp3/kernels.rs` run, which return the same integers, so host and
device decode the same samples. `mp3::kernel_selftest()` runs both on
random input (`firmware/rust_audio_bench` runs it).

| kernel | use |
|---|---|
| `dot16` | the two 16-point halves of the synthesis DCT |
| `dot24` | the IMDCT, 18 inputs padded to 24 |
| `window` | the synthesis window, 8 outputs per vector |

- The dot kernels keep x in q0-q5 and stream the coefficient rows through
  `esp.vmulas.s16.xacc.ld.ip`; `esp.srs.s.xacc` reads each 40-bit sum out
  with its shift and saturation.
- The window runs all high halves with SAR 0, then all low halves with SAR
  16 through `esp.vmul.s32.s16xs16`, as the AAC QMF windows do. The rows'
  offsets are immediates in the kernel and must follow `WINDOW_OFF`.
- The AAC decoder's notes on this chip apply (`docs/aac.md#pie-kernels`):
  a `fence` after vector stores, the registers PIE can address with, and CFG
  not being saved per task, so every decode call runs `mp3_k_prepare()`.

## Checking it

`test/run.sh` decodes the material with `audio_framework` and holds this decoder to its output sample by sample.
