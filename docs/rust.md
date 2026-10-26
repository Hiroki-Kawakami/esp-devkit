# Rust

Rust crates build inside ordinary IDF components, next to C/C++, without Cargo.
`rustc` runs from the IDF CMake/ninja build; one component is one crate, and the
crate graph follows the components' `REQUIRES`/`PRIV_REQUIRES`. Crates are
`#![no_std]` with `alloc`. The simulator build does not take Rust crates yet.

## Usage

`devkit.cmake` includes `tools/rust/rust.cmake`, so any component can call
`devkit_rust_crate()` after `idf_component_register()`:

```cmake
# C and Rust in one component
idf_component_register(
    SRCS "hcd_ll.c"
    INCLUDE_DIRS "inc"
    PRIV_REQUIRES bsp rust_rt)
devkit_rust_crate(ROOT "src/lib.rs")
```

```cmake
# Rust only: IDF needs a source to create a static library
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES app rust_rt)
devkit_rust_crate(ROOT "main.rs")
```

- The crate is named after the component. Every component that builds a crate
  must (PRIV_)REQUIRES `rust_rt`.
- Crates of the component's requirements are passed with `--extern`; a crate
  `use`s them by component name. Requirements without a crate are ignored.
- `sdkconfig` is a crate of constants: `bool`, `i32` (int), `u32` (hex) and
  `&str`. Each enabled bool option is also a `--cfg`, so
  `#[cfg(CONFIG_SPIRAM)]` works.
- `EDITION` defaults to 2024.
- `app_main` in Rust is `#[unsafe(no_mangle)] pub extern "C" fn app_main()`. The
  main component keeps `WHOLE_ARCHIVE ON` as with C.

`firmware/rust_hello` is a working example: a Rust-only main and an app
component mixing C and Rust.

## Build

- rustc emits the crate's object into the build's `rust/` directory, and the
  object is archived into the component library, so linker fragments that
  name the component's archive cover the Rust code too. Multiple codegen units
  are merged with `ld -r`.
- rustc's dep-info becomes the ninja depfile, with `-Zbinary-dep-depinfo`
  adding the dependency crates' `.rmeta`. A C change does not rerun rustc; a
  crate change reruns its dependents only if its metadata changed.
- `-Copt-level` follows `CONFIG_COMPILER_OPTIMIZATION_*` (none 0, debug 1,
  size s, perf 2); incremental compilation is on for 0 and 1.
  `-Cdebug-assertions` follows `CONFIG_COMPILER_OPTIMIZATION_ASSERTIONS_*`.

| IDF target | Rust target |
|---|---|
| esp32 | `xtensa-esp32-none-elf` |
| esp32s3 | `xtensa-esp32s3-none-elf` |
| esp32p4, esp32s31 | `riscv32imafc-unknown-none-elf` |
| esp32c5, esp32c6, esp32c61, esp32h2, esp32h21 | `riscv32imac-unknown-none-elf` |

## Runtime (`libs/rust_rt`)

- The allocator shim (`__rust_alloc` and friends) forwards to `malloc`/`free`,
  and to `memalign` above 4-byte alignment. rustc only generates the shim when
  it links the final binary, which it never does here.
- The panic handler formats the message and calls `esp_system_abort`, which
  prints it with a backtrace.
- It links the sysroot's `alloc`, `core` and `compiler_builtins`, with `-lgcc`
  ahead of `compiler_builtins` so the intrinsics both define come from libgcc.
  `compiler_builtins` only contributes what libgcc lacks, such as the 128-bit
  `__udivti3`/`__umodti3`.

## Toolchain (`nix/rust.nix`)

- rustc is the esp-rs fork (`esp-rs/rust-build`), the only one with Xtensa. It
  is a nightly channel, so `-Z` flags and unstable features need no
  `RUSTC_BOOTSTRAP`. The release has no x86_64-darwin build.
- The release ships std for the host only. The derivation builds `core`,
  `compiler_builtins` and `alloc` from its rust-src for each Rust target above
  and installs them into the toolchain's own sysroot, so a plain
  `rustc --target <triple>` finds them.
- bindgen is nixpkgs' bindgen rebuilt with the `runtime` feature and wrapped to
  load esp-clang's libclang (`esp-clang-libs` in `nix/toolchains.nix`), which
  parses the Xtensa targets.

### compiler_builtins patch

`nix/patches/compiler-builtins-no-libm.patch` stops `compiler_builtins` from
exporting libm.

`compiler_builtins` exports C libm functions (`sinf`, `powf`, `sqrtf`, `fmaf`,
...) on every target except Windows and Apple, and no feature or cfg turns
that off. IDF links with g++, which moves `-lc`/`-lm` to the end of the link
line, after the sysroot rlibs. Without the patch these exports satisfy the
firmware's C references before picolibc does. They are generic software
implementations, while picolibc implements `sqrtf`, `fmaf`, `fminf`/`fmaxf`,
`fabsf` and others with FPU instructions on RISC-V (and `sqrtf` on Xtensa).

## Upgrading

The toolchain is pinned by `releaseTag` and the tarball hashes in
`nix/rust.nix`. On a version bump:

1. **The patch must still apply.** The derivation fails if it does not. The
   exports live in `library/compiler-builtins/compiler-builtins/src/math/mod.rs`:
   the `libm_intrinsics!` macro and any function defined directly in an
   `intrinsics!` block there.
2. **Check that no C names are exported.** On each target's
   `libcompiler_builtins.rlib`, `nm -g --defined-only` must show only `__`
   intrinsics, `_R` (mangled Rust) and `anon.*.llvm.*` symbols. A new plain C
   name is either a new libm export the patch has to cover or something else
   that can displace libc.
3. **compiler_builtins cfgs.** The derivation passes by hand what Cargo's
   `build.rs` would set (`compiler-builtins/compiler-builtins/build.rs`,
   `compiler-builtins/libm/configure.rs`). Compare with those files for added
   or renamed cfgs. Feature `mem` stays off; libc/ROM provide `memcpy` and
   friends.
4. **Allocator shim signatures.** The functions in `libs/rust_rt/src/lib.rs`
   must match the `extern "Rust"` block at the top of
   `library/alloc/src/alloc.rs` (names, argument types such as `Alignment` and
   `NonNull<u8>`, and the `__rust_no_alloc_shim_is_unstable_v*` suffix) and
   `__rust_alloc_error_handler`. Nothing type-checks the two against each
   other: a missing name fails the link, a changed type does not.
5. **Unstable features used by rust_rt** (`rustc_attrs`'s
   `#[rustc_std_internal_symbol]`, `ptr_alignment_type`) may change with
   nightly.
6. **esp-clang** is pinned from IDF's `tools/tools.json` and follows the IDF
   version, not rustc.

A new IDF target needs its Rust target in `_devkit_rust_triple`
(`tools/rust/rust.cmake`) and, if new, in the `targets` list of `nix/rust.nix`.
The Rust target's float ABI must match IDF's `-mabi` (ilp32f on P4/S31, ilp32
on C6).
