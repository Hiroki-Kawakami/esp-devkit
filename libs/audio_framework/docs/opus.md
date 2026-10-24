# Opus decoder (`audf_opus`)

`inc/audf_opus.h`: Opus (RFC 6716 with the RFC 8251 updates), SILK, CELT and
Hybrid, plain C on every target.

## Scope

- Channel mapping family 0 only (mono or stereo, from the OpusHead), so
  `.opus` files with more channels are refused at create.
- **The output is S16 at 48 kHz, whatever the packet's bandwidth.** CELT
  always runs at 48 kHz internally; a narrower bandwidth is the same 48 kHz
  signal with the upper bands left empty, so decoding at a lower rate only
  adds a resampler. SILK runs at 8, 12 or 16 kHz and is resampled up.
- The OpusHead output gain is applied. The pre-skip is not: the caller knows
  where the stream starts (`audf_opus_decoder_pre_skip()`), as it knows where
  a seek lands.
- A packet of length 0 is a lost packet. CELT conceals it with noise in the
  last band energies, fading out; SILK keeps the last frame's LPC and pitch
  with decaying gain, on noise for unvoiced frames. Neither is the
  reference's PLC, and FEC (LBRR) data is skipped, not used.
- `audf_opus_decoder_final_range()` is the range coder state after the last
  packet, the value `opus_demo` writes next to each packet, so test vectors
  check the entropy decoding exactly.

## Sources

RFC 6716's prose describes SILK fully, and SILK is written from it. For CELT
the prose leaves out procedures the bitstream depends on (bit allocation
interpolation, PVQ split decisions, the theta and stereo rules), and the RFC
makes its Appendix A code normative. CELT therefore follows Appendix A for
tables and for every bit-exact procedure, with the code structure our own.
Tables carried over keep the RFC's BSD-3 notice (`src/audf_opus_tables.c`,
`src/audf_opus_silk_tables.c`).

Two SILK parts follow Appendix A too, although the prose would allow
otherwise:

- **SILK reconstruction uses the reference's fixed-point formats** (Q14
  excitation and LPC state, the gain adjustment and rewhitening of the LTP
  state through an int16 buffer, `INVERSE32_varQ`/`DIV32_varQ`, saturating
  LPC accumulation as in libopus 1.6). A float version is close enough on the
  test vectors but drifts audibly on low-bitrate real files, since the LTP
  feeds the error back frame after frame. With these formats SILK-only output
  is bit-exact against fixed-point libopus.
- **The SILK resampler copies the reference's** (2x all-pass up, then an
  8-tap fractional FIR, with its 0/4/7-sample delay). The filter is not
  normative but its delay is (RFC 6716 Table 54), since the encoder delays
  CELT to match it; with the reference's filter, SILK and Hybrid output can be
  compared against libopus sample for sample.

## Numbers

- CELT is fixed point so its loops can move to PIE: band signals are Q20
  (`NORM_SHIFT`), time samples PCM × 1024 (`SIG_SHIFT`, saturated at
  2^29 − 1). Products take the high word of a 32 × 32 multiply (`opus_mulh`,
  one `mulh` on RV32) against Q31 twiddles and rotations.
- Scalars (band gains, energies, the PVQ normalisation) are float, in files
  built with `-ffp-contract=off -fno-math-errno` like `audf_aac`.
- The IMDCT is a DCT-IV through an N/4-point complex FFT (radix 4, 2, 3 and
  5 stages) with block floating point: the input is scaled so its L1 norm
  stays below 2^30, so no stage can overflow.
- **The post-filter weights are rounded to Q15 as the reference does**, and
  each output adds a constant (+6 while fading, +3 steady, +1 in the
  de-emphasis) for the floor of the high-word multiplies. The comb filter is
  recursive, so a one-sided rounding error builds up; without the correction
  the test vectors lost about 3 points of `opus_compare` quality.
- The PVQ codebook sizes V(n, k) are a 2092-entry table built at create.

Memory is about 175 KB for stereo and 127 KB for mono, all from
`alloc_caps`. The decoding stack stays under 2.7 KB, so a 4 KB task runs it.

## Speed

Decoding a clip held in PSRAM on an ESP32-P4 rev 1 at 360 MHz, with the
decoder in PSRAM. Cost is a percentage of one core; the clips are from
`test/make_opus_material.sh`, and esp_opus_dec is esp_audio_codec 2.5.0.

| clip | audf_opus | esp_opus_dec | stack |
|---|---|---|---|
| `music_128k` (CELT, stereo, 20 ms) | 12.8 % | 10.4 % | 2652 B |
| `speech_16k_stereo` (SILK and Hybrid) | 10.4 % | 7.8 % | 2136 B |
| `speech_8k` (SILK, mono) | 2.7 % | 1.8 % | 1736 B |

On CELT, band decoding (PVQ, theta, folding) takes about 43 % of the time
and the IMDCT about 31 %, the post-filter 8 % and the de-emphasis 5 %.
Allocating the decoder in internal RAM fails; it is too large.

## Checking it

The host tool `test/opus_dec_test.c` reads `opus_demo`'s `.bit` format
(32-bit length and final range per packet) or an Ogg Opus file:

```sh
D=esp-devkit/libs/audio_framework
nix develop -c gcc -std=c11 -O2 -ffp-contract=off -fno-math-errno \
    -I $D/inc -I $D/src -I esp-devkit/idf_compat/include \
    $D/test/opus_dec_test.c $D/src/audf_opus*.c $D/src/audf_alloc.c $D/src/audf_codec.c \
    -lm -o opus_dec_test
./opus_dec_test [--mono] testvector01.bit out.raw
./opus_dec_test --ref in.ref.raw in.opus out.raw
```

For `.bit` it reports final range mismatches (exit 1); `--mono` decodes to
one channel. For Ogg it drops the pre-skip and trims the end to the last
granule position. `--ref` prints SNR and the largest difference against
48 kHz s16le, `--loops n` decodes again for timing.

Material:

- Test vectors: the RFC 8251 set at `https://opus-codec.org/docs/`
  (`opus_testvectors-rfc8251.tar.gz`), `testvectorNN.bit` with `.dec` (stereo)
  and `m.dec` (mono) outputs. Compare with `opus_compare` from RFC 6716
  Appendix A (`opus_compare -s -r 48000 ref.dec out.raw`, without `-s` for
  mono).
- Synthetic: `test/make_opus_material.sh <dir>` encodes music- and
  speech-like signals with ffmpeg's libopus at several bitrates and frame
  sizes, a file whose packets span pages and one tagged with a cover by
  opusenc, and writes ffmpeg's decode of each as `<name>.ref.raw`.

All 12 vectors decode with no final range mismatch, stereo and mono, and pass
`opus_compare` (quality 97.8-100 % stereo, 96.1-98.5 % mono). Against the
synthetic material's references: CELT 66-72 dB, SILK-only bit-exact or
within two LSB, Hybrid about 72 dB. ffmpeg's libopus is a float build, and
`speech_60ms` differs from it by up to 1948 (49 dB) where the float SILK
takes another path; a fixed-point libopus build matches ours there to the
sample.
