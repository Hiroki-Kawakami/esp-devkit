# AAC decoder (`audf_aac`)

`inc/audf_aac.h`: AAC-LC with SBR (HE-AAC v1) and
parametric stereo (v2), with PIE kernels on the ESP32-P4 and a C build
everywhere else. Both builds produce the same samples.

## Scope

- AAC-LC, one or two channels, 1024-sample frames, raw (AudioSpecificConfig)
  or ADTS. A PCE is accepted when it describes at most two channels.
- SBR at twice the core rate. PS is the baseline decoder: 20-band hybrid
  filterbank, IID/ICC and IPD/OPD. 34-band parameters are folded onto the 20
  bands, so such streams (conformance `al_sbr_ps_06`) play but not to spec.
- Not supported: 960-sample frames, more than two channels, downsampled SBR,
  AAC Main/SSR/LTP and the ER object types.
- **The output format is fixed at create.** With SBR enabled, a core of 24 kHz
  or less always decodes at twice the rate, even before or without SBR data,
  and `AUDF_AAC_HE_V2` always gives stereo. HE-AAC is often signalled only
  inside the frames, and a format change after the first frame would make the
  caller reopen its output. `AUDF_AAC_HE_OFF` plays an HE stream's core at the
  core rate; `AUDF_AAC_HE_V1` plays a PS stream as mono.

## Numbers and bit-exactness

- Spectra are `16 x` the dequantized value, clamped to 2^29. The IMDCT's
  DCT-IV is scaled by 1/(2M). Time samples are PCM x 8, which keeps three
  fraction bits through the windows and the SBR analysis. Coefficients are
  Q15; data stays 32-bit, and a 32x16 product is taken from 16-bit halves
  (`aac_split`) so PIE can do it.
- The control path is float: the TNS filter, SBR envelopes, gains and HF
  generation, and in PS the hybrid filterbank, band powers, transient gains
  and mixing matrices. These files are built with
  `-ffp-contract=off -fno-math-errno` (`CMakeLists.txt`). A fused multiply-add
  on one target and not the other changes the result, and with errno `sqrtf`
  and `lrintf` become library calls on the device.
- Every PIE kernel has a C twin in `src/audf_aac_kernels_c.c` with the same
  integer result, floors included. `audf_aac_kernel_selftest` runs both on
  random input. Since host and device output match, a hash of the host's PCM
  checks a device run.

## PIE kernels

| kernel | use |
|---|---|
| `aac_k_qmf_analysis`, `aac_k_qmf_synthesis` | SBR QMF windows, two MAC passes (hi with SAR 0, lo with SAR 16) |
| `aac_k_dct4_64x4` | four DCT-IV(64) side by side in the four 32-bit lanes |
| `aac_k_dct4_1024` | the long-window IMDCT |
| `aac_k_syn_in4`, `aac_k_syn_v4` | QMF synthesis input and output reordering for four slots |
| `aac_k_pack` | rounding, saturation and interleaving to S16 |
| `aac_k_ps_allpass`, `aac_k_ps_gain`, `aac_k_ps_mix`, `aac_k_ps_mix_real` | PS decorrelation, transient gain and mixing |
| `aac_k_ps_unzip`, `aac_k_ps_zip` | QMF rows to and from the PS lane layout |

- **QMF analysis is a DCT-IV(64).** The 32 complex subbands are
  `(C[k], -C[63 - k])`, where C is the DCT-IV of the window output u
  reordered as `u63, u62, -u0, u61, -u1, ..., -u30, u32`. So analysis and
  synthesis share `aac_k_dct4_64x4`, four slots per call.
- **The 1024-point IMDCT runs in lanes too.** Its FFT512 is split by
  `n mod 4` into four FFT128s, one per lane, with the stage loop of the 64-point
  kernel. A 4x4 transpose and a radix-4 step across the lanes finish it. The
  result is left as `x[k] = X[2k]`, `x[512 + k] = X[1023 - 2k]`, which the
  windowing reads directly, so there is no interleave pass. Q15 twiddles cost
  about 1 dB of SNR against Q31 (83 to 82 dB on LC).
- Short windows (DCT-IV(128), Q31) stay in C; they are rare.
- **PS keeps each slot in lanes**: four subbands per group stored as
  `[re x4, im x4]`, so one 128-bit load holds a complex group. The 71 hybrid
  subbands sit in lanes 0-9 and 11-71; lane 10 stays empty so the QMF bands
  4-63 land on whole groups and need only `vunzip`/`vzip`. The mixing matrix
  is interpolated per subband as Q30 accumulators and used as Q14, rounded.
  Streams without IPD/OPD (fdk-aac writes none) take the real-only mix.
  Against the float version this costs at most 0.7 dB of SNR.
- **Twiddles are stored once per vector**, as `[wr x4, wi x4]`. `CMUL` gets
  `[wi x4, wr x4]` by loading the two 64-bit halves the other way round. Half
  the table bytes measurably helps HE-AAC on PSRAM (see below).

PIE facts found on this chip (rev 1) that the documentation does not state:

- **Stores need a `fence` before scalar code reads them.** Without one, a
  scalar load right after a kernel returns can see the old contents. It
  showed up as output that changed from run to run. Every kernel that stores
  ends with `fence`.
- `esp.srcmb.s16.qacc` wraps; it does not saturate.
- QACC holds four 64-bit lanes, stored in the order L.L, L.H, H.L, H.H.
- `esp.vmul.s16` and `esp.vmul.s32.s16xs16` shift the product right by SAR,
  rounding down. The 32-bit form writes lanes 0-3 to its first destination and
  lanes 4-7 to its second.
- Only s0, s1, a0-a5, s8-s11 and t3-t6 work as PIE address registers (not sp,
  t0-t2, a6 or a7).
- FreeRTOS saves SAR with a task's PIE context but not CFG. `aac_k_prepare`
  sets the misaligned-access bits and SAR = 0 on every decode call.
- Two rounds of `esp.vzip.32` on register pairs give a 4x4 transpose
  (`TRANSPOSE4`).

## Memory and where the time goes

Everything comes from `alloc_caps`, except the 12 KB work buffer when
`scratch_caps` asks for something else (it falls back when that heap is
full). Measured on an ESP32-P4 rev 1 at 360 MHz
(128 KB L2, 64-byte lines), decoding synthetic clips from
`test/make_aac_material.sh` back to back. Cost is a percentage of one core;
esp_audio_codec 2.5.0 uses its default allocations.

| stream | memory | PSRAM | all internal | esp_audio_codec |
|---|---|---|---|---|
| AAC-LC 44.1 kHz stereo 128 kbit/s | 51 KB | 3.0 % | 3.0 % | 4.0 % |
| HE-AAC v1 48 kHz stereo 64 kbit/s | 127 KB | 11.0 % | 8.6 % | 10.3 % |
| HE-AAC v2 48 kHz 32 kbit/s | 111 KB | 10.2 % | 8.1 % | 13.8 % |

esp_audio_codec keeps about 17 KB in internal RAM (its blocks under the 16 KB
`SPIRAM_MALLOC_ALWAYSINTERNAL` limit) and about 91 KB in PSRAM for HE-AAC.

Cycles per core frame, PSRAM:

| stream | core | filterbank | QMF analysis | SBR | PS | QMF synthesis |
|---|---|---|---|---|---|---|
| HE-AAC v1 | 294k | 200k | 245k | 566k | - | 368k |
| HE-AAC v2 | 175k | 134k | 115k | 239k | 514k | 386k |

- **On PSRAM, the bytes a frame touches matter more than the instructions.**
  Once the per-frame working set plus the code exceeds the L2, each
  frame starts cold: the filterbank of the v2 stream's one channel costs twice
  what an LC channel costs, for the same code. Hence:
  - PS runs four slots at a time between the SBR rows and the synthesis. A
    38-slot frame buffer would be 39 KB and costs about 0.6 M cycles per frame.
  - With SBR, the time samples live in the scratch buffer, each channel going
    through filterbank and analysis before the next one starts; the analysis
    writes `x_low` only up to `max(kx, kx_prev)`; the twiddle tables are half
    size. Together these are worth 0.3-0.45 M cycles per HE-AAC frame.
- **Compiler flags do not help.** `-O3` is no faster than `-O2` and `-Os` is
  slower. Code placement alone moves results by about ±5 %.
- **Putting only the 12 KB scratch buffer in internal RAM** saves about 0.1 M
  cycles per frame on v1 and 0.16 M on v2 when nothing else runs, and about
  2 % of the decode time next to H.264 playback.
- **Next to video decoding everything costs more.** During H.264 playback of an
  HE-AAC stereo clip with a 22.05 kHz core, decoding took 7.4 % of a core
  without SBR and 17 % with it, about 1.7 times the cost of the same work
  alone; the core stage (bitstream, Huffman tables) grew the most, about 2.5
  times. esp_audio_codec had been seen at about 13 % and 40 % there.
- PS is bound by instructions (about one per cycle). Its float band powers
  (PIE multiplies only 16-bit values, too coarse for the transient detector)
  and hybrid filterbank are the largest parts left.

## Checking it

Material:

- Synthetic: `test/make_aac_material.sh <dir>` encodes LC, HE v1 and v2 clips
  at several rates with fdk-aac from a temporary nix-shell (unfree, so
  nothing is added to a flake).
- Conformance: the ISO/IEC 14496-26 bitstreams at
  `https://standards.iso.org/ittf/PubliclyAvailableStandards/ISO_IEC_14496-26_2010_Bitstreams/`,
  `DVD1/mpeg4audio-conformance/compressedMp4/` (`al*.mp4`, `al_sbr_*.mp4`) with
  reference WAVs under `referencesWav/`.

The host decoder `test/aac_dec_test.c` reads ADTS or MP4:

```sh
D=esp-devkit/libs/audio_framework
nix develop -c gcc -std=c11 -O2 -ffp-contract=off -fno-math-errno \
    -I $D/inc -I $D/src -I esp-devkit/idf_compat/include \
    $D/test/aac_dec_test.c $D/src/audf_aac*.c $D/src/audf_alloc.c $D/src/audf_codec.c \
    -lm -o aac_dec_test
nix develop -c ffmpeg -i in.m4a -f f32le ref.f32
./aac_dec_test --he v2 --ref ref.f32 in.m4a out.s16
```

`--he off|v1|v2` picks the mode. `--ref` prints SNR, the largest difference
and the lag against float PCM. `--hash` prints the FNV-1a of the PCM, `--loops
n` decodes again after a reset. Keep `-ffp-contract=off`: GCC fuses
multiply-adds in its GNU modes, and a fused build's hash differs from the
device's. For a mono stream, ffmpeg's reference needs `-af pan=mono|c0=c0`.

Against ffmpeg's float decoder: synthetic LC 81-84 dB, conformance LC
65-84 dB, HE v1 72-82 dB, v2 76-80 dB.
Against the ISO reference WAVs: `al_sbr_e`/`gh` 79.6 dB, `al_sbr_i` 74.8 dB,
`al_sbr_s` 72.3 dB.
