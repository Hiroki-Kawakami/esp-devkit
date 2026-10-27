# Rust

Rust crates build inside ordinary IDF components, next to C/C++, without Cargo.
`rustc` runs from the IDF CMake/ninja build; one component is one crate, and the
crate graph follows the components' `REQUIRES`/`PRIV_REQUIRES`. Crates are
`#![no_std]` with `alloc`. The same component CMakeLists build the crates into
the host simulator.

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

## C headers

```cmake
devkit_rust_crate(ROOT "lib.rs"
    BINDGEN_HEADERS "bsp.h" "freertos/FreeRTOS.h" "freertos/task.h" "rust_hello.h"
    EXPORT_HEADERS "rust_hello_export.h")
```

```rust
mod ffi {
    #![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
```

- Headers are named as they are `#include`d, in include order, and bindgen
  parses them with the component's include directories and definitions plus
  IDF's build-wide ones. Only items declared in a listed header are bound,
  with the types they use. An API declared in a header that a listed header
  includes needs that header listed too (`xTaskCreatePinnedToCore` is in
  `freertos/idf_additions.h`, not `freertos/task.h`).
- `static inline` functions get a C wrapper compiled into the component, so
  they are callable like any other function. Function-like macros are not
  bound.
- `EXPORT_HEADERS` are C APIs implemented in Rust. Each function they declare
  must be defined at the crate root (or re-exported there) with the
  declaration's signature; a missing definition or a different signature
  fails the build. A header is either all Rust or not listed here.
- Bindings are regenerated when a header changes and the crate rebuilds only
  if the bindings did.

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

## Simulator and host tests

In the simulator build the crates target the host (`rustc -vV`'s host triple)
and their objects link into the `simulator` executable with the C/C++ sources.
The idf_component_register shim records `REQUIRES`/`PRIV_REQUIRES`, so crate
dependencies are the same as on the device. A Rust main component defining
`app_main` works there as on the device: without `MAIN_SRCS`, the simulator's
`main()` calls it (`firmware/rust_hello/simulator`).

- The simulator's Kconfig lacks IDF's options (`CONFIG_FREERTOS_HZ`,
  `CONFIG_SPIRAM`, ...). `#[cfg(CONFIG_*)]` on such an option is false there
  without a warning (`-Aunexpected_cfgs`); the device build still checks the
  names. `sdkconfig::` constants exist only for options in the build's Kconfig.
- Values that C gets from macros (`pdMS_TO_TICKS()`, `configTICK_RATE_HZ`) are
  not bound; wrap them in a C function in the component.
- bindgen takes the host compiler's system include directories from the
  compiler itself, since the Nix wrapper supplies them.

`devkit_rust_crate(... TESTS)` adds `rust_test_<crate>`, which builds the
crate's `#[test]`s with `rustc --test` and runs them; `rust_test` runs all of
them. The crate root must be `#![cfg_attr(not(test), no_std)]`: the test
harness needs std. Tests run with `-Zpanic-abort-tests`. rustc links the
executable from the Rust crates and an archive of idf_compat
(`devkit_sim_idf_compat`, compiled with the simulator's flags), so tests can
call the IDF and FreeRTOS APIs idf_compat provides. Other C, including the
component's own sources and bindgen's `static inline` wrappers, is not linked:
code reached from a test cannot call it, and code that only non-test paths reach
is dropped by the linker. On the device `TESTS` does nothing.

```sh
cmake --build firmware/rust_hello/simulator/build --target rust_test
```

## Editor

Configuring a build (device or simulator) writes `rust-project.json` into its
build directory: every crate with its root, dependencies, `--cfg`s, target and
`OUT_DIR`, plus the toolchain's sysroot. Point rust-analyzer at the build being
worked on, e.g. in Zed's `.zed/settings.json`:

```json
{
  "lsp": {
    "rust-analyzer": {
      "initialization_options": {
        "linkedProjects": ["firmware/rust_hello/esp32p4/build/rust-project.json"]
      }
    }
  }
}
```

The devShell (not the CI image) provides `rust-analyzer`. Zed prefers a
rust-analyzer on the project environment's `PATH` over the one it downloads,
and takes that environment from the shell (direnv included), which also puts
the toolchain's `rustc` there for rust-analyzer's target queries. The toolchain
has no cargo; rust-analyzer logs that `cargo metadata` failed on the sysroot
and loads it without.

## Crates

### `devkit` (`libs/devkit`)

IDF-independent utilities, `core` only.

`bitflags!` declares a flags type with a subset of the bitflags 2.x syntax:

```rust
devkit::bitflags! {
    pub struct Caps: u32 {
        const DMA = ffi::MALLOC_CAP_DMA;
        const INTERNAL = ffi::MALLOC_CAP_INTERNAL;
        const DMA_INTERNAL = Self::DMA.bits() | Self::INTERNAL.bits();
    }
}

let caps = Caps::DMA | Caps::INTERNAL;
unsafe { ffi::heap_caps_malloc(size, caps.bits()) };
```

- The type is `#[repr(transparent)]` over any primitive integer, signed ones
  included. `Clone`, `Copy`, `PartialEq`, `Eq`, `Hash` and `Debug` are always
  implemented; derive others (`PartialOrd`, `Ord`, `Default`) as needed.
- `empty`, `all`, `bits`, `from_bits` (`None` if a bit no flag defines is
  set), `from_bits_truncate`, `from_bits_retain`, `is_empty`, `is_all`,
  `contains`, `intersects`, `union`, `intersection`, `difference`,
  `symmetric_difference`, `complement`, `insert`, `remove`, `toggle`, `set`,
  all `const fn`; operators `| & ^ - !` and the assigning forms.
- Only `complement`/`!` and `from_bits_truncate` drop undefined bits; every
  other operation keeps them.
- `Debug` prints `Caps(DMA | INTERNAL | 0x40)`, `Caps(0x0)` when empty. Flags
  are named in declaration order when all their bits are set and some are not
  yet named; the rest is printed in hex.
- Not provided: `iter`/`iter_names`, parsing, the `Flags` trait, unnamed
  (`const _`) flags.

`guard(value, f)` runs `f(value)` when the guard goes out of scope;
`Guard::into_inner(g)` returns the value without running `f`. The guard
derefs to the value. `defer! { ... }` runs statements at the end of the
scope and cannot be cancelled.

```rust
pub fn open() -> Result<Sensor, EspError> {
    let bus = devkit::guard(new_bus()?, |b| unsafe { ffi::i2c_del_master_bus(b); });
    let dev = devkit::guard(add_device(*bus)?, |d| unsafe { ffi::i2c_master_bus_rm_device(d); });
    check(unsafe { ffi::sensor_reset(*dev) })?;
    Ok(Sensor { bus: Guard::into_inner(bus), dev: Guard::into_inner(dev) })
}
```

- Guards run in reverse order of creation, on scope end, `return` and `?`,
  never on panic (panic=abort).
- `let _ = guard(...)` drops, and so runs, the guard immediately; bind it to
  a name.

`FmtBuf<N>` is a `core::fmt::Write` target in an `N`-byte array, for
formatting without the heap and for passing the result to C:

```rust
let path = FmtBuf::<64>::from_fmt(format_args!("{}/{}", MOUNT, name));
if path.is_truncated() {
    return Err(...);
}
unsafe { ffi::fopen(path.as_c_str().as_ptr(), c"rb".as_ptr()) };
```

- The text is always NUL-terminated, so it holds `N - 1` bytes. `as_c_str()`
  ends at the first NUL, `as_str()` keeps everything written.
- On overflow it keeps what fits, cut at a character boundary, sets
  `is_truncated()` and returns `Err`, so `write!` stops formatting. Later
  writes are ignored until `clear()`.
- It derefs to `str`; `Display` and `Debug` format it as a string.

`ByteReader` and `ByteWriter` read and write integers and floats of either
byte order over a slice, failing with `OutOfBounds`:

```rust
let mut r = ByteReader::new(data);
let size = r.u32_be()? as usize;
let kind = r.array::<4>()?;
let mut body = r.sub(size - 8)?;
let timescale = body.at(20)?.u32_be()?;

let mut w = ByteWriter::new(&mut buf);
let len_at = w.reserve(2)?;
w.put_u16_le(cid)?;
w.put_bytes(payload)?;
w.set_u16_le(len_at, (w.position() - len_at - 2) as u16)?;
send(w.written());
```

- `u8`, `i8`, `{u,i}{16,32,64}_{le,be}`, `u24_{le,be}` (as `u32`) and
  `f{32,64}_{le,be}`; the writer has `put_` and `set_` for each. A failed
  read or write leaves the position and the buffer unchanged.
- `bytes`, `rest` and `sub` borrow from the data, not the reader.
- `sub(n)` returns a reader over the next `n` bytes and skips them; `at(offset)`
  returns a reader from `offset` of the reader's data, without moving it.
- `reserve(n)` writes `n` zeros and returns their position; `set_*` overwrite
  bytes already written. The u24 writers drop the high byte.

`StaticLease<T>` lends a `static` value to one owner at a time, e.g. a large
internal-RAM buffer that subsystems hand over to each other:

```rust
#[repr(C, align(64))]
struct SharedSram([u8; SHARED_SRAM_BYTES]);

static SRAM: StaticLease<SharedSram> = StaticLease::new(SharedSram([0; SHARED_SRAM_BYTES]));

let mut lease = SRAM.take().ok_or(Busy)?;
let (front, back) = lease.0.split_at_mut(SHARED_SRAM_BYTES / 2);
```

- `take()` returns `None` while a `Lease` is alive and never waits. Dropping
  the `Lease` returns the value; the next owner sees what was left in it.
- Size and alignment come from `T`. A zero-initialized `static` lands in
  `.bss`, which IDF places in internal RAM.
- Pointers passed to C from a lease are not tracked: C must be done with them
  before the `Lease` is dropped.

### `idf` (`libs/idf`)

Safe wrappers for ESP-IDF APIs. `idf::sys` is the raw bindgen output, public
for APIs `idf` does not wrap yet.

`idf::Error` is a non-zero `esp_err_t`; wrapped APIs return
`idf::Result<T>`. For a C call that returns `esp_err_t`:

```rust
idf::Error::check(unsafe { ffi::bsp_init(&config) })?;
if let Some(err) = idf::Error::from_code(rc) { ... }
```

- Constants for the generic codes in `esp_err.h` (`Error::NO_MEM`,
  `Error::TIMEOUT`, ...); component-specific codes belong to their crates.
- `Display` prints `ESP_ERR_NO_MEM (0x101)`. `name()` is `esp_err_to_name()`,
  `UNKNOWN ERROR` on the device without `CONFIG_ESP_ERR_TO_NAME_LOOKUP`.

`idf::log::{error, warn, info, debug, verbose}!` log in IDF's format, so the
lines read like `ESP_LOGx` output in `idf.py monitor`:

```rust
use idf::log::info;

const TAG: &CStr = c"app";
info!(TAG, "fps={fps:.1}");
```

- A level above `CONFIG_LOG_MAXIMUM_LEVEL` compiles to nothing; otherwise the
  tag's runtime level (`esp_log_level_get`) is checked before the arguments
  are evaluated. `log::enabled(level, tag)` is the same check.
- The message is formatted into a 256-byte stack buffer, and again into a
  `String` if it does not fit. Each line is one `esp_log_write` (log v1, with
  the prefix built from `CONFIG_LOG_COLORS` and
  `CONFIG_LOG_TIMESTAMP_SOURCE_*`) or one `esp_log` (log v2, which adds the
  prefix itself).
- Binary log mode (`CONFIG_LOG_MODE_BINARY_EN`) is not supported.
- The macros are exported at the crate root as `__log_*` only because
  `macro_rules!` cannot be exported from a module; use the `idf::log` paths.
- In the simulator, idf_compat prints up to Info, without colors.

`idf::time::Instant` is `std::time::Instant` over `esp_timer_get_time()`, with
microsecond resolution, plus `since_boot()`. `idf::system::restart()` calls
`esp_restart()`; in the simulator it exits with status 0.

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
- It adds `-z noexecstack` to the link: Rust objects carry `.note.GNU-stack`
  and Xtensa GCC's do not, which ld otherwise warns about.
- On the host it uses the toolchain's own `alloc`/`core` (no
  `compiler_builtins`; the host libgcc/compiler-rt covers it), `malloc` and
  `posix_memalign`, and prints the panic to stderr before `abort()`. The host
  sysroot is built with panic=unwind, so `rust_rt` defines an empty
  `rust_eh_personality` for its unwind tables.

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
  parses the Xtensa targets. The wrapper also passes `-resource-dir`: loaded
  at runtime on Linux, libclang does not find its builtin headers. libclang
  gets the toolchain's libc headers as `-isystem` (`picolibc/include` with
  `CONFIG_LIBC_PICOLIBC`, then `<triple>/include`); gcc finds them through
  IDF's `-specs`.
- The CI firmware image (`mkFwImage`) carries rustc and bindgen with the rest
  of the devShell's IDF environment.
- rustc and bindgen are looked up when CMake configures; after a toolchain
  update, reconfigure (`idf.py reconfigure`, or `cmake -S ... -B ...` for the
  simulator) so the build uses the new paths and rebuilds every crate.

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
7. **bindgen** (nixpkgs) writes a top-level `const` parameter of a `static
   inline` function into its C wrapper as a pointee `const`. The wrapper
   source is compiled with `-Wno-duplicate-decl-specifier
   -Wno-discarded-qualifiers` for that (`_devkit_rust_bindgen` in
   `tools/rust/rust.cmake`); drop them once bindgen generates correct
   wrappers.

A new IDF target needs its Rust target in `_devkit_rust_triple`
(`tools/rust/rust.cmake`) and, if new, in the `targets` list of `nix/rust.nix`.
The Rust target's float ABI must match IDF's `-mabi` (ilp32f on P4/S31, ilp32
on C6).
