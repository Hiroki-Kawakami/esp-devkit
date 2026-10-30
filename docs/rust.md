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
- `allocator_api` is enabled in every crate (`-Zcrate-attr`), so
  `Vec<T, A>`, `Box::new_in` and the like need no `#![feature]`.
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

`RESOURCES <definition>` generates `$OUT_DIR/resources.rs` from a resgen
definition; see docs/resgen.md.

## Hot crates

`OPT_LEVEL` is for crates whose loops are the point of the crate. It mirrors what
the C codecs do with `-O2`:

```cmake
devkit_rust_crate(ROOT "src/lib.rs" TESTS OPT_LEVEL 2)
```

- The crate builds at that level in one codegen unit, whatever the project's
  optimization, on the device, in the simulator and in `rust_test`. A project at
  `CONFIG_COMPILER_OPTIMIZATION_NONE` keeps `-O0`.
- On the device it also turns `debug-assertions`, and so overflow checks, off;
  they cost 4-14 % on the decompressors at IDF's default of assertions on. Host
  tests keep them, so they catch arithmetic that only wraps by accident:
  write `wrapping_*` where wrapping is meant.
- Only `#[inline]` and generic functions cross a crate boundary, and they compile
  at the calling crate's level. A hot loop lives in the crate that owns it, not
  in a helper of a dependency, and goes through `dyn` only at block rate.
- Hold bit-reader and cursor state in locals for the length of a loop rather
  than behind `&mut self`, and keep the refill path `#[cold]`.
- `unsafe` indexing is for the loops a profile names. Masks (`i & (N - 1)`) on
  arrays and `chunks_exact`/`zip` on slices remove most checks without it.

### Assembly kernels

Keep the `.S` in the component and select it with the target. A constant
generated next to the selection keeps both branches compiled on every target;
`--check-cfg` knows only the current target's Kconfig options, so `cfg` on
another target's option warns.

```cmake
set(srcs "/dev/null")
set(have_pie false)
if(IDF_TARGET STREQUAL "esp32p4")
    set(srcs "src/kernels_p4.S")
    set(have_pie true)
endif()
idf_component_register(SRCS ${srcs} PRIV_REQUIRES devkit idf rust_rt)
devkit_rust_out_dir(out_dir)
file(CONFIGURE OUTPUT "${out_dir}/pie.rs" CONTENT "pub(crate) const HAVE_PIE: bool = ${have_pie};\n" @ONLY)
devkit_rust_crate(ROOT "src/lib.rs" TESTS OPT_LEVEL 2)
```

```rust
include!(concat!(env!("OUT_DIR"), "/pie.rs"));

mod p4 {
    unsafe extern "C" {
        pub fn k_dot(out: *mut i32, x: *const i16, c: *const i16, rows: i32);
    }
}

pub fn dot(out: &mut A16<[i32; 32]>, x: &A16<[i16; 32]>, c: &A16<[i16; 32]>, rows: usize) {
    assert!(rows <= 32);
    if HAVE_PIE {
        unsafe { p4::k_dot(out.as_mut_ptr(), x.as_ptr(), c.as_ptr(), rows as i32) };
    } else {
        dot_scalar(out, x, c, rows);
    }
}
```

- A kernel and its scalar version have one safe signature and write the same
  bytes; the lengths are checked once at this boundary, and `A16<T>` makes
  16-byte alignment a type.
- Tables a `.S` refers to by name are `#[unsafe(no_mangle)] pub static` of an
  `A16` type.
- A kernel's task stack and every buffer it stores to come from
  `Caps::SIMD` alone, and each decode entry sets the kernel's `CFG` and `SAR`
  state: these chips keep them global across tasks.

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
- RISC-V targets pass `-Ztune-cpu=rocket-rv32`: LLVM has no scheduling model for
  these cores and this one is what GCC schedules for. Instruction selection is
  unaffected.

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

Tests that need generated data read it at run time from
`$DEVKIT_TEST_FIXTURES`. The crate's `test/run.sh` generates the files into a
temporary directory (with whatever `nix-shell -p` brings in, so the generators'
dependencies stay out of the flake), sets the variable and builds the
`rust_test_<crate>` target of a simulator project that reaches the crate:

```sh
nix develop -c libs/devkit_compression/test/run.sh
```

A custom command can generate files for a crate to `include_bytes!`, into the
directory `devkit_rust_out_dir()` names:

```cmake
devkit_rust_out_dir(out_dir)
add_custom_command(OUTPUT "${out_dir}/data.bin" COMMAND ...)
devkit_rust_crate(ROOT "lib.rs" DEPENDS "${out_dir}/data.bin")
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

`A16<T>` puts `T` on a 16-byte boundary and derefs to it. `Zeroable` marks a
type whose all-zero bytes are valid, which `idf::heap` needs to build one
directly on the heap; implement it (`unsafe`) on plain structs.

`devkit::math` is the C math library, which `core` leaves out: `sinf`, `cosf`,
`powf`, `log2f`, `ldexpf`, `sqrtf`, `floorf`, `ceilf`, `roundf`, `lrintf`,
`lroundf`, and `sin`, `cos`, `pow`, `log2`, `atan2`, `sqrt`, `floor`, `round`,
`lround`. `abs` and `copysign` are methods in `core`.

- `sqrtf`, `floorf`, `ceilf` and `roundf` are compiler intrinsics, which inline
  on a target with an FPU and otherwise call picolibc. `lrintf` is one
  `fcvt.w.s` where the target has it.
- The rest call picolibc. Each `f64` one is software on a target with a
  single-precision FPU.
- `lrintf`, `lroundf` and `lround` return `i32`, whatever `long` is on the host.

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
`idf::system::CORE_COUNT` is `CONFIG_FREERTOS_NUMBER_OF_CORES` and `core_id()`
the core running the caller, which an unpinned task may leave right after the
call.

`idf::fs::File` is a file open for reading, over `open`/`read`/`lseek`/`fstat`:

```rust
let file = File::open(c"/sdcard/a.mp4")?;
let size = file.size()?;
file.seek(offset)?;
let n = file.read(&mut buf)?;
```

- All methods take `&self`, and `read` and `seek` share one position, so tasks
  sharing a `File` serialize a seek with its read. `read` may return less than
  asked; 0 is the end of the file. Closed on drop.

`idf::heap::Caps` is the `MALLOC_CAP_*` bitflags and is itself a
`core::alloc::Allocator` over `heap_caps_*`:

```rust
use idf::heap::{self, Caps};

let mut buf: Vec<u8, Caps> = Vec::with_capacity_in(4096, Caps::DMA | Caps::INTERNAL);
let frame = Box::new_in([0u16; 320 * 240], Caps::SPIRAM);
let free = heap::free_size(Caps::DEFAULT);
```

- `MALLOC_CAP_8BIT`/`32BIT` are `ACCESS_8BIT`/`ACCESS_32BIT`. `EXEC` exists
  only with `CONFIG_HEAP_HAS_EXEC_HEAP`, as in C.
- Alignments above 4 bytes (the TLSF guarantee) use
  `heap_caps_aligned_alloc`/`_calloc`; `grow`/`shrink` use `heap_caps_realloc`
  otherwise. Any `Caps` value frees any block.
- `heap::zeroed_box::<T>(caps)` and `heap::zeroed_slice::<T>(len, caps)` allocate
  a `Zeroable` value zeroed, without passing it through the stack, and return
  `NO_MEM` where `Box::new_in` would panic. `heap::try_vec(capacity, caps)` is an
  empty `Vec` reserved exactly, also `NO_MEM` on failure.
- `free_size`, `minimum_free_size`, `largest_free_block`, `total_size` and
  `info` take the same caps. `esp_get_free_heap_size()` is
  `free_size(Caps::DEFAULT)`, `esp_get_free_internal_heap_size()` is
  `free_size(Caps::ACCESS_8BIT | Caps::DMA | Caps::INTERNAL)` and
  `esp_get_minimum_free_heap_size()` is `minimum_free_size(Caps::DEFAULT)`.
- In the simulator the caps are ignored and the size queries report a fixed
  256 MiB.

`idf::ppa::Srm` is a PPA client for scale-rotate-mirror transfers, on chips with
a PPA and in the simulator, where `start` finishes the transfer on the CPU.
`start` queues a transaction and returns; `wait` takes the oldest one.
`new(n)` allows `n` in flight. The source is copied whole, rotation is
counter-clockwise, and the destination's address and size must be cache-line
aligned. One client per task; the engine serves clients in turn.

`idf::thread` follows `std::thread` on FreeRTOS tasks:

```rust
let h = thread::Builder::new().name("decoder").stack_size(16 * 1024).priority(6)
    .core(Some(1)).stack_caps(Caps::SPIRAM).spawn(move || decode(input))?;
let frames = h.join();
```

- `spawn(f)` uses the defaults (name `task`, 8192-byte stack, priority 5, no
  core affinity) and panics if the task cannot be created;
  `Builder::spawn` returns `Err(Error::NO_MEM)` instead. Names longer than 15
  bytes are cut.
- `join()` returns the value directly (a panic aborts). Dropping the handle
  detaches the task.
- `stack_caps` creates the task with `xTaskCreatePinnedToCoreWithCaps`. When
  such a task ends, IDF starts a short-lived task to free its stack.
- `thread::scope(|s| ...)` follows `std::thread::scope`: tasks from `s.spawn`
  or `Builder::spawn_scoped(s, f)` may borrow from the caller, and all of them
  have ended when it returns, joined or not.
- `current()` returns the calling task as a `Thread`: `name`, `priority`,
  `set_priority`, `core_affinity` (`None` if not pinned) and
  `stack_high_water_mark` (bytes). It is not `Send`, so it cannot outlive the
  task. In the simulator the high water mark is the requested stack size, and
  the thread running `app_main` is not a task (empty name, no stack).
- `sleep(d)` waits at least `d`: it rounds up to ticks of `CONFIG_FREERTOS_HZ`
  and adds one, since `vTaskDelay(n)` can end up to a tick early. The same
  rounding applies to every timeout in `idf::sync`. `yield_now()` is
  `vTaskDelay(0)`.

`idf::sync` wraps FreeRTOS queue objects, created in `new` (`try_new` returns
`NO_MEM` instead of panicking). In a `static`, wrap them in `LazyLock`:

```rust
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static CONFIG: OnceLock<Config> = OnceLock::new();
let cfg = CONFIG.get_or_try_init(load_config)?;
```

- `OnceLock` and `LazyLock` follow std. `get_or_try_init` leaves the cell
  empty when `f` fails, so a later call retries. A task that finds another
  one initializing waits a tick at a time (`vTaskDelay(1)`), not spinning, so
  a lower-priority initializer still runs. Initializing the same cell from its
  own `f` waits forever.
- `OnceLock::get` never waits and is the only one usable from an ISR: `None`
  while uninitialized or being initialized.

- `Mutex<T>`: a FreeRTOS mutex (priority inheritance). `lock()` returns the
  guard directly; the guard is not `Send`, as the task that took the mutex
  must give it.
- `Semaphore::new(initial, max)`: a counting semaphore; `new(0, 1)` is a
  binary one. `give()` returns `false` at `max`.
- `channel::<T>(capacity)`: a bounded FreeRTOS queue; values are moved in and
  out by copy. Senders and receivers can both be cloned; there is no
  disconnection, so `recv()` waits even when every sender is gone. Values
  left in the queue are dropped with the last handle.
- `give_from_isr`, `try_take_from_isr`, `try_send_from_isr` and
  `try_recv_from_isr` are for interrupt handlers and request the context
  switch themselves when they wake a higher-priority task. The other methods
  must not be called from an ISR.

### `devkit_compression` (`libs/devkit_compression`)

Streaming inflate and LZW; see its README.

### `devkit_gfx` (`libs/devkit_gfx`)

2D rendering, IDF-independent and host-testable. Memory comes from the
allocators passed in; output goes to a `Surface` over any memory.

```rust
static BODY: FontChain = FontChain::new(&[&resources::BODY_24, &resources::JP_24]);

let corner = CornerMask::new_in(12, Caps::INTERNAL);
let mut cache = GlyphCache::new_in(128 * 1024, Caps::SPIRAM);
let mut run = TextRun::new();
run.layout(&BODY, "Download", &TextOptions::default());

let mut list = DisplayList::new();
list.fill(screen, Color::hex(0xffffff));
list.round_rect(button, corner, Color::hex(0x2196f3));
list.text(24, 20, &run, Color::hex(0x1d2126));

let mut prepared = Prepared::new();
prepared.prepare(&list, damage.regions(), &mut cache);
let surface = unsafe { Surface::new(strip_ptr, stride, strip) };
draw::<Rgb565, _, _>(&list, &prepared, &surface);
```

- `prepare` resolves the glyphs of a frame and runs on one task. `draw` only
  reads, so several tasks can draw disjoint surfaces at once. It must stay
  inside the regions given to `prepare`.
- A mask stays valid until the next `prepare`. The glyphs of one frame must fit
  the cache; `Prepared::missing` counts those that did not, which are not drawn.
- `CornerMask` and `ShadowMask` are generated once and never freed; keep them
  with the theme. `Op::Shadow` leaves the middle of its rectangle unpainted, so
  something opaque must cover it.
- Damage merges into at most `N` rectangles, and several rectangles covering
  75% promote to the full area.
- `Pixel` implements a framebuffer format: `Rgb565` and `Bgr888` (B, G, R in
  memory).

### `devkit_ui` (`libs/devkit_ui`)

UI framework on `bsp_rs`. A UI is a tree of `Component`s; `firmware/ui_demo` is a
working example.

```rust
#[derive(Clone, Copy)]
enum Msg { Open(u16), ToggleMenu }

impl Component for FileBrowser {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, cx: &mut Cx<Self>) {
        match msg {
            Msg::Open(i) => self.selected = Some(i),
            Msg::ToggleMenu => self.menu = !self.menu,
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        ui.column([
            ui.row([
                ui.text(&TITLE, "Download").color(INK).grow(1),
                ui.icon(&ICONS, icons::MORE).id("more").on_tap(Msg::ToggleMenu),
            ]).height(80).align(Align::Center),
            ui.list(0, self.names.len(), 80).grow(1),
        ])
    }

    fn list_row<'a>(&'a self, _list: ListId, i: usize, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        ui.row([ui.text(&BODY, &self.names[i])]).on_tap(Msg::Open(i as u16))
    }
}

let app = App::new(display, board.touch, FileBrowser::new(), &AppConfig::default())?;
app.run();
```

- `update` runs when a message arrives and `view` when it called
  `cx.rebuild()`. `view` only builds descriptions in per-rebuild memory, which is
  matched against the kept nodes; only what differs is laid out and painted.
- Messages are `Copy`, at most 8 bytes, and handlers take values, so a rebuild
  allocates nothing for them. Larger payloads stay in the state.
- `Child` is a component made by a parent's `view`: `ui.child::<C>(props)`, or
  `ui.child_mapped` to turn its `cx.emit` events into the parent's messages. It
  keeps its state and rebuilds only when its props differ or it asks to.
- Widgets are functions returning an `Element`; the primitives are boxes, text,
  images, `scroll` and `list`.
- `list` builds only the rows in view, through `Component::list_row`. Scrolling
  moves the rows without running `view`. A rebuild of the owner rebuilds them,
  unless `revision` is given and did not change.
- `pressed_background` changes without a rebuild.
- Sizes are `i16` pixels. A row, column or grid places children by `grow`,
  `gap`, `align` and `justify`; `absolute` takes a child out of the flow.
- `Handle::post` sends a message to the root component from any task.
- `Probe` serves test harnesses: `tree`, `tap` (through the touch path, refused
  when the node is covered), `invoke` (the handler directly), `idle` and `stats`.

Memory goes where `Config` says: `data_caps` for nodes, text, display lists and
masks, `strip_caps` for the strips and `lane_stack_caps` for the lane stacks;
`AppConfig` has the glyph cache. Strip memory can change while running, for a UI
that gives way to something faster:

```rust
static SRAM: StaticLease<SharedSram> = StaticLease::new(SharedSram([0; BYTES]));

// Video starts: strips move to PSRAM and the decoder gets the internal RAM.
let sram = handle.set_strips(Strips::allocate(128 * 1024, Caps::SPIRAM)?, 1)?;
decoder.start(sram.into_inner::<Lease<SharedSram>>());
// And back.
handle.set_strips(Strips::new(decoder.stop()), 2)?;
```

- `set_strips` takes effect between frames and returns the memory in use, with
  nothing drawing in it any more. Before `run`, `App::set_strips` does the same;
  from the UI task itself `Handle::set_strips` would wait forever.
- Memory is split into two buffers per lane, and each must hold 16 rows of the
  logical width.

`Presenter` puts a `DisplayList` on the panel; `App` runs it, and it can be used
alone:

```rust
let mut presenter = Presenter::new(display, &Config::default())?;
loop {
    presenter.acquire()?;
    let dirty = apply_input();
    presenter.invalidate(dirty);
    build(&mut list);
    presenter.render(&list, &mut cache)?;
}
```

- Damaged rows are drawn into internal RAM strips, by one task per core, and
  copied into a framebuffer with `Blitter`, rotated as `Config::rotation` says.
- `PresentMode::Swap` flips at a frame boundary without tearing. With three
  framebuffers a new frame starts drawing while the last one waits for the
  boundary; with two, it waits too. `PresentMode::InPlace` overwrites the
  framebuffer on the panel. It answers up to a frame sooner, and tears.
- Damage is tracked per framebuffer: a frame repaints what changed since that
  framebuffer was last drawn, and nothing is copied between framebuffers.
- Damage rows are aligned to a 16 row grid.
- Call `acquire` before reading input: it may wait for a framebuffer. `render`
  takes it by itself otherwise.
- A stalled copy logs `blit stalled` every second and keeps waiting.

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
   nightly. Once `allocator_api` is stable, drop its `-Zcrate-attr` and
   `-Aunused_features` from the common rustc arguments in
   `tools/rust/rust.cmake`.
6. **esp-clang** is pinned from IDF's `tools/tools.json` and follows the IDF
   version, not rustc.
7. **bindgen** (nixpkgs) writes a top-level `const` parameter of a `static
   inline` function into its C wrapper as a pointee `const`. The wrapper
   source is compiled with `-Wno-duplicate-decl-specifier
   -Wno-discarded-qualifiers` for that (`_devkit_rust_bindgen` in
   `tools/rust/rust.cmake`); drop them once bindgen generates correct
   wrappers.

## ESP32-P4 code generation

GCC builds for `rv32imafc_zicsr_zifencei_zaamo_zalrsc_zca_zcf_xesploop_xespv2p1`
and Rust for plain `riscv32imafc`. Neither compiler emits the hardware loops or
the PIE instructions from C or Rust source, so the two see the same instruction
set. rustc's `-Ctarget-cpu=esp32p4` adds Zba, Zbb, Zbs and Zcb, which IDF's GCC
does not enable for this chip; it stays off.

A new IDF target needs its Rust target in `_devkit_rust_triple`
(`tools/rust/rust.cmake`) and, if new, in the `targets` list of `nix/rust.nix`.
The Rust target's float ABI must match IDF's `-mabi` (ilp32f on P4/S31, ilp32
on C6).
