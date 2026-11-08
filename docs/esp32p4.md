# ESP32-P4

What the chip does to code and data paths, measured on Tab5 (P4 rev 1.x,
360 MHz CPU, 200 MHz PSRAM, IDF v6.1) unless a line says otherwise.

## CPU

The core is RV32IMAFC: no 64-bit integer unit and no double-precision FPU.
`uint64_t / uint32_t` becomes a `__udivdi3` call, and every `double` becomes
`__adddf3` / `__muldf3` / `__divdf3` / `floor` / `llround`. A host CPU hides
both, so the stage proportions of a host profile do not carry over: JPEG decode
ran 115x slower than on an M-series host, but a box resize ran 320x slower
because of one `__udivdi3` per destination sample in its inner loop.

Before optimizing a file for P4, compile it with the pinned toolchain from the
devShell and look at what it calls:

```sh
riscv32-esp-elf-gcc -march=rv32imafc -mabi=ilp32f -O2 -c file.c -o /tmp/f.o
riscv32-esp-elf-nm -u /tmp/f.o
riscv32-esp-elf-objdump -dr /tmp/f.o | grep R_RISCV_CALL
```

Rust code generation for the chip is in [Rust](rust.md#esp32-p4-code-generation).

## PIE

- Allocate every buffer a PIE kernel stores to with `MALLOC_CAP_SIMD` alone,
  never `MALLOC_CAP_SIMD | MALLOC_CAP_INTERNAL`. The heap matches capabilities
  one priority level at a time and `RETENT_RAM` lists the two on different
  levels, so the combination only matches the small `RAM` regions.
  `MALLOC_CAP_INTERNAL` alone can land in RTC RAM, where a PIE store faults.
- Task stacks too: the lazy vector save writes 128-bit registers to the stack.
- `CFG` and `SAR` are global, not saved per task. Set the misaligned
  load/store bits and `SAR = 0` at the entry of each call that runs kernels;
  a kernel that touches `SAR` writes it back to 0 before returning.
- `.ip`/`.xp` address and scalar operands cannot use `t0`-`t2`, `a6`, `a7`.
- `esp.vsld.16` with a negative amount shifts right arithmetically (the sign
  bit comes in), although the reference says zero fill; mask the fields after it.
- `vmul.s16` saturates; arrange kernels so no product exceeds 16 bits.
- PIE has no float operations. Float code that must stay bit-exact gets no kernel.
- Several PIE tasks on one core work on IDF v6.1.

## Memory and caches

- Internal SRAM is behind the cache too (`esp_cache_get_alignment` for
  `INTERNAL | DMA` is 64). DMA descriptors in internal RAM need a C2M sync, or
  the DMA never runs.
- Code from flash and data from PSRAM share the 128 KB L2. A line refetch costs
  about 1500 cycles, so touched data size and flash-resident tables matter more
  than arithmetic. In the HE-AAC SBR decoder, copying a 4 KB noise table from
  flash into decoder memory cut its slot stage from 130M to 106M cycles, while
  caching 15 KB per channel of intermediate data evicted code and slowed the
  rest about 35 %. `CONFIG_CACHE_L2_CACHE_256KB` drops HE-AAC by 28 % but costs
  128 KB of internal RAM; the firmware in this repository keep 128 KB.
- Single-run percentages move about ±5 % with heap placement. Average over
  several heap offsets before believing a small gain.
- PSRAM bandwidth: CPU memcpy about 42 MB/s, PPA 80-100 MB/s. A PPA transfer
  and a concurrent CPU memcpy share it and finish in about 70 % of their back to
  back time. A full 720x1280 RGB565 write into PSRAM takes about 20 ms.

## PPA and JPEG

- PPA SRM time is quantized to 16-row macroblocks on rev < 3 (fixed 16x16):
  4 to 16 rows take about 300 µs and 17 to 32 rows about 590 µs at 1280 wide,
  so 8-row strips double the hardware time. Rev ≥ 3 defaults to 32-row blocks;
  strip heights tuned to 16 need re-measuring there.
- The JPEG RX descriptor chain pauses safely at an unspliced tail: with a ring
  of 1 or 2 strips and consumer delays up to 20 ms per strip the output stays
  bit-exact; only the decode timeout bites. Throughput with 2 strips equals 5.
- A recycled RX descriptor pool must hold at least ring + 1 descriptors; a
  single self-looping descriptor asserts in `dma2d.c` (`fsm_idle`).
- Floating point on an ISR path faults ("Coprocessors must not be used in ISRs").

## Chip revision

- On rev < 3 the PSA ECDSA hardware driver logs "not supported on this chip
  revision" on every verify before falling back to software. Set
  `CONFIG_MBEDTLS_HARDWARE_ECDSA_VERIFY=n` in firmware for those chips.
