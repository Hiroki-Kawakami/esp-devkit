# Opus decoder (`devkit_audio::opus`)

`src/opus/`: Opus (RFC 6716 with the RFC 8251 updates), SILK, CELT and
Hybrid, scalar on every target.

## Scope

- Channel mapping family 0 only (mono or stereo, from the OpusHead), so
  `.opus` files with more channels are refused at create.
- **The output is S16 at 48 kHz, whatever the packet's bandwidth.** CELT
  always runs at 48 kHz internally; a narrower bandwidth is the same 48 kHz
  signal with the upper bands left empty, so decoding at a lower rate only
  adds a resampler. SILK runs at 8, 12 or 16 kHz and is resampled up.
- The OpusHead output gain is applied. The pre-skip is not: the caller knows
  where the stream starts (`OpusDecoder::pre_skip()`), as it knows where
  a seek lands.
- A packet of length 0 is a lost packet. CELT conceals it with noise in the
  last band energies, fading out; SILK keeps the last frame's LPC and pitch
  with decaying gain, on noise for unvoiced frames. Neither is the
  reference's PLC, and FEC (LBRR) data is skipped, not used.
- `OpusDecoder::final_range()` is the range coder state after the last
  packet, the value `opus_demo` writes next to each packet, so test vectors
  check the entropy decoding exactly.

## Sources

RFC 6716's prose describes SILK fully, and SILK is written from it. For CELT
the prose leaves out procedures the bitstream depends on (bit allocation
interpolation, PVQ split decisions, the theta and stereo rules), and the RFC
makes its Appendix A code normative. CELT therefore follows Appendix A for
tables and for every bit-exact procedure, with the code structure our own.
Tables carried over keep the RFC's BSD-3 notice (`src/opus/tables.rs`,
`src/opus/silk_tables.rs`).

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
  2^29 − 1). Products take the high word of a 32 × 32 multiply (`mulh`,
  one `mulh` on RV32) against Q31 twiddles and rotations.
- Scalars (band gains, energies, the PVQ normalisation) are float, in files
  built with `-ffp-contract=off -fno-math-errno` like the AAC decoder.
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
`caps`. The decoding stack stays under 2.7 KB, so a 4 KB task runs it.

## Checking it

`test/run.sh` decodes the material with `audio_framework` and holds this decoder to its output sample by sample.
