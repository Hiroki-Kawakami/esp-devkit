# AAC decoder (`devkit_audio::aac`)

`src/aac/`: AAC-LC with SBR (HE-AAC v1) and
parametric stereo (v2), with PIE kernels on the ESP32-P4 and a scalar build everywhere else. Both builds produce the same samples.

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
  (`split`) so PIE can do it.
- The control path is float: the TNS filter, SBR envelopes, gains and HF
  generation, and in PS the hybrid filterbank, band powers, transient gains
  and mixing matrices. These files are built with
  `-ffp-contract=off -fno-math-errno` (`CMakeLists.txt`). A fused multiply-add
  on one target and not the other changes the result, and with errno `sqrtf`
  and `lrintf` become library calls on the device.
- SBR HF generation (`lpc`, `estimate`, `gains`, `slot`) has no PIE kernels. PIE
  has no float operations and these must round as the float decoder does, so
  the cost there is memory: `estimate` converts each `x_low` sample once, and the
  helpers that run once a frame are `#[optimize(size)]`, since their cost is the
  lines fetched.
- HE-AAC touches about as much as the 128 KB L2 holds, and that L2 also holds code
  from flash at about 1500 cycles a line. Each PSRAM line a frame adds evicts
  code, so a table walked every frame belongs in decoder memory, and a buffer
  added for speed has to win back what the core and PS lose. `kernels::Tables`
  copies the QMF, DCT and long window tables out of flash on chips with PIE when
  SBR is on (14 KB), and `Sbr` holds the noise table (4 KB). The PS tables stay
  in flash. `x_high` is not cached between `estimate` and `slot` in this L2
  size: 15 KB per channel slows the core and filterbank by more than `slot` gains.
- Every PIE kernel has a scalar twin in `src/aac/kernels.rs` with the same
  integer result, floors included. `aac::kernel_selftest` runs both on
  random input. Since host and device output match, a hash of the host's PCM
  checks a device run.

## PIE kernels

| kernel | use |
|---|---|
| `qmf_analysis`, `qmf_synthesis` | SBR QMF windows, two MAC passes (hi with SAR 0, lo with SAR 16) |
| `dct4_64x4` | four DCT-IV(64) side by side in the four 32-bit lanes |
| `dct4_1024` | the long-window IMDCT |
| `syn_in4`, `syn_v4` | QMF synthesis input and output reordering for four slots |
| `pack` | rounding, saturation and interleaving to S16 |
| `ps_allpass`, `ps_gain`, `ps_mix`, `ps_mix_real` | PS decorrelation, transient gain and mixing |
| `ps_unzip`, `ps_zip` | QMF rows to and from the PS lane layout |

- **QMF analysis is a DCT-IV(64).** The 32 complex subbands are
  `(C[k], -C[63 - k])`, where C is the DCT-IV of the window output u
  reordered as `u63, u62, -u0, u61, -u1, ..., -u30, u32`. So analysis and
  synthesis share `dct4_64x4`, four slots per call.
- **The 1024-point IMDCT runs in lanes too.** Its FFT512 is split by
  `n mod 4` into four FFT128s, one per lane, with the stage loop of the 64-point
  kernel. A 4x4 transpose and a radix-4 step across the lanes finish it. The
  result is left as `x[k] = X[2k]`, `x[512 + k] = X[1023 - 2k]`, which the
  windowing reads directly, so there is no interleave pass. Q15 twiddles cost
  about 1 dB of SNR against Q31 (83 to 82 dB on LC).
- Short windows (DCT-IV(128), Q31) stay scalar; they are rare.
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
- FreeRTOS saves SAR with a task's PIE context but not CFG. `prepare`
  sets the misaligned-access bits and SAR = 0 on every decode call.
- Two rounds of `esp.vzip.32` on register pairs give a 4x4 transpose
  (`TRANSPOSE4`).

## Checking it

`test/run.sh` decodes the material with `audio_framework` and holds this decoder to its output sample by sample.
