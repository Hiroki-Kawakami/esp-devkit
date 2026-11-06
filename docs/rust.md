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
component mixing C and Rust, ending in async code on `idf::executor`.

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
- `BINDGEN_OPTIONS` go to bindgen as they are, e.g. `--blocklist-type=<name>`
  for a struct whose layout bindgen gets wrong, which the crate then defines
  itself next to the `include!`.
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

`devkit::executor` runs futures on tasks the platform provides, behind two
traits: `Park` (wait on the running task, wake it from anywhere, tick count) and
`CriticalSection` (keeps out other tasks, cores and interrupts). `idf` provides
them for FreeRTOS.

```rust
let local = LocalExecutor::new(&CRITICAL, park);
local.spawn_local(async {
    let spawner = executor::spawner().await;
    let job = spawner.spawn(async { 1 + 1 });
    assert_eq!(job.await, 2);
});
local.run();
```

- `LocalExecutor` runs on the task that created it, so its futures need not
  be `Send`; `run` never returns, `run_until(future)` returns that future's
  output. `Executor` is shared by every task that calls `run` on it, one per
  core typically, and takes `Send` futures only. `block_on` runs one future on
  the calling task with a throwaway `LocalExecutor`.
- `LocalExecutor::run_ready()` runs what is ready and returns the ticks to the
  next timer, for a task that waits on something else; its `Park` is never
  parked, and `unpark` has to end that wait.
- `spawn` returns a `JoinHandle`, itself a future for the output. Dropping it
  lets the task run on; `abort` drops the future at its next suspension point.
- `spawner().await` and `local_spawner().await` reach the executor running the
  caller, from its waker; they panic elsewhere.
- Spawners and wakers work from any task; a waker also from an interrupt, by
  reference: dropping the last waker of a finished task frees it, which an
  interrupt must not do.
- A blocking call stalls every task of the executor it runs on.
- Dropping an executor drops its unfinished tasks; awaiting one of them panics.
- `sleep(ticks)`, `sleep_until(tick)` and `timeout(ticks, future)` count the
  `Park`'s ticks: a timer is due on the executor's first look at or after its
  tick, and runners wait no longer than the next one. Ticks wrap, so a wait
  must stay under half their range. `timeout` drops the future when it runs
  out.
- `Signal<T>` keeps the latest value until `wait().await` takes it;
  `channel(critical, capacity)` is a bounded queue with one `Receiver`
  (`recv().await`, `try_recv`) and any number of `Sender`s: `try_send` never
  waits, `send(v).await` waits for room in turn. `signal` and `try_send` work
  from interrupts as long as the waiting waker clones without allocating, as
  devkit's do: they wake a copy and leave the stored waker in place. Once the
  `Receiver` is dropped, `is_closed()` is true, `try_send` gives the value
  back and `send(v).await` returns at once, dropping it; `recv()` still waits
  with every sender gone.
- `oneshot::channel(critical)` carries one value from any task: `send` gives
  it back once the `Receiver` is dropped, which `is_closed()` tells a worker
  that can stop early, and the `Receiver` is a future that gives
  `Err(Closed)` once the `Sender` is dropped without sending.

`devkit::sync` has `Mutex` and `RwLock` over a `Platform` (the calling task,
its priority, park/unpark, a clock, a critical section); `idf::sync` fixes it
to FreeRTOS.

```rust
static COUNT: Mutex<u32> = Mutex::new(0);
*COUNT.lock() += 1;
*COUNT.lock_async().await += 1;
```

- `new` is `const` and allocates nothing. Taking a free lock is one
  compare-and-swap; a waiter spins only while the holder runs on another core,
  then parks.
- `lock()`/`read()`/`write()` block the calling task, and so every future of
  an executor it runs; `lock_async()`/`read_async()`/`write_async()` wait as a
  future. Tasks and futures queue on the same lock. Guards are `Send` and may
  be dropped elsewhere, so a future may hold one across `.await`.
- Waiters queue by priority (a future by its runner's), first come first
  within one. An unlock wakes the first waiter and lets anyone take the lock
  meanwhile, unless that waiter has waited 1 ms or more: then the lock is
  handed to it. There is no priority inheritance.
- `RwLock` prefers writers: once one waits, new readers wait too.
- Dropping a waiting future passes on what was meant for it.
- Not usable from an ISR.

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
call. `in_isr()` is always `false` in the simulator. `idf::system::random()` is `esp_random()`, from the OS entropy source in
the simulator.
`idf::system::app_description()` is `esp_app_get_description()`: `version()`
(`PROJECT_VER`, the project's `git describe` unless it sets one),
`project_name()`, `time()`, `date()` and `idf_version()` as `&'static str`. In
the simulator `idf_version()` is `simulator`.

`idf::nvs::init()` initializes the default NVS partition for every user of it
and may be called by each of them; it does the work once. A partition without
free pages or written by a newer IDF is erased first, losing what it held.
`Nvs::open` initializes it as well and opens a namespace of 15 bytes at most,
created when missing. A `Key<T>` names a value and its type:

```rust
const VOLUME: Key<u8> = Key::new(c"volume");
const PATH: Key<String> = Key::new(c"path");

let mut nvs = Nvs::open(c"app")?;
let volume = nvs.get(VOLUME)?.unwrap_or(80);
nvs.set(PATH, "/sdcard/a.mp3")?;
nvs.commit()?;
```

`Key::new` panics past 15 bytes, so a `const` key that long fails the build.
Integers up to 64 bits, `bool` (a `u8`), `String` (set from `&str`) and
`Vec<u8>` (a blob, set from `&[u8]`) are values; an app type becomes one by
implementing `Value` over one of them, `from_raw` returning `None` for what it
cannot take. `get` is `Ok(None)` for a missing key, one written as another
type, or a value `from_raw` refuses, and `Err` for anything else. What `set`
writes lasts once `commit` has returned; `remove` takes a missing key as done.

`idf::fs` follows `std::fs` over the POSIX calls, with `&CStr` paths:

```rust
let file = File::open(c"/sdcard/a.mp4")?;
let size = file.size()?;
file.seek(offset)?;
let n = file.read(&mut buf)?;

File::create(c"/sdcard/cache.bin")?.write_all(&data)?;
let log = OpenOptions::new().append(true).create(true).open(c"/sdcard/log.txt")?;
for entry in fs::read_dir(c"/sdcard")? {
    let dir = entry.is_dir();
    let name = entry.file_name();
}
fs::rename(c"/sdcard/a.tmp", c"/sdcard/a.bin")?;
```

- `File` methods take `&self`, and `read`, `write` and `seek` share one
  position, so tasks sharing a `File` serialize a seek with the access after
  it. `read` and `write` may move less than asked; `read` returning 0 is the
  end of the file. `sync` writes buffered data through. Closed on drop.
- `metadata`, `create_dir`, `remove_file`, `remove_dir` and `rename` complete
  the set; `read_dir` skips `.` and `..`. Failures are `FAIL`, without the
  reason.

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

`idf::cache` is for memory a DMA reads or writes: `msync(ptr, len, Flags)`
is `esp_cache_msync` with `Flags::{C2M, M2C, INVALIDATE, UNALIGNED}`,
`alignment(caps)` the cache line of that memory (4 where it is not cached), and
`uncached(ptr)` the non-cacheable alias of internal RAM, through which the CPU
never shares a cache line with a DMA (`ptr` itself on chips without one). In
the simulator `msync` does nothing.

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
  task. In the simulator the high water mark is the requested stack size.
- `sleep(d)` waits at least `d`: it rounds up to ticks of `CONFIG_FREERTOS_HZ`
  and adds one, since `vTaskDelay(n)` can end up to a tick early. The same
  rounding applies to every timeout in `idf::sync`. `yield_now()` is
  `vTaskDelay(0)`.

`idf::sync` has `devkit::sync`'s `Mutex` and `RwLock` on FreeRTOS, and wraps
FreeRTOS queue objects (`Semaphore`, `channel`), created in `new` (`try_new`
returns `NO_MEM` instead of panicking). In a `static`, wrap those in
`LazyLock`:

```rust
static DONE: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(0, 1));
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

- `Spinlock<T>` is a `portMUX_TYPE` around `T`, for state shared with
  interrupt handlers: `Spinlock::new` is `const`, and `with(|t| ...)` is the
  only way to reach `T`, from a task or an ISR. Interrupts on that core stay
  off inside `f`, so it must not block or allocate; calling `with` on the same
  lock inside `f` panics. The simulator has one critical section for all.
- `Mutex<T>` and `RwLock<T>` park on bit 1 of the notification value. A task
  that waited on one must end through `idf::thread`, which waits for a
  lock's last wakeup to reach it before deleting it.
- `idf::freertos::Mutex<T>` is a FreeRTOS mutex, for a lock that needs
  priority inheritance. `lock()` returns the guard directly; the guard is not
  `Send`, as the task that took the mutex must give it.
- `idf::freertos::Timer` is a one-shot FreeRTOS software timer:
  `Timer::one_shot(name, period, callback)`, and `reset` starts its period
  over. The callback runs on the timer task.
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

`idf::executor` runs `devkit::executor` on FreeRTOS tasks:

```rust
let local = thread::Builder::new().name("ui").priority(4).spawn_local_executor(|local| {
    local.spawn_local(ui_loop());
})?;
let shared = thread::Builder::new().name("work").stack_size(16 * 1024).spawn_executor()?;
shared.spawn(fetch());
let answer = executor::block_on(async { local.spawn(compute()).await });
```

- `spawn_local_executor` starts one task with a `LocalExecutor` and runs
  `setup` on it first; `spawn_executor` starts one task per core, pinned, on one
  `Executor`. Both run for good and return a `Spawner`.
- `block_on` runs a future on the calling task.
- `sleep(d)` and `timeout(d, future)` wait at least `d`, rounded up to ticks
  plus one as `thread::sleep` does.
- `channel(capacity)` and `Signal::new(&CRITICAL)` hand values from tasks and
  interrupts to async code, `oneshot::channel()` one value from a task; the
  other way, async code sends on `idf::sync::channel` as any task does.
- `tick_duration(ticks)` is how long `run_ready`'s ticks are.
- `TaskPark` waits for bit 0 of the task's notification value at index 0 and
  wakes from interrupts too; `TaskPark::current()` makes one for the calling
  task without allocating; `CRITICAL` is one spinlock shared by every
  executor.
- Rust code shares notification index 0 as bits, one per kind of waiter
  (bit 0 executors, bit 1 `idf::sync` locks). A
  Rust task must not wait on index 0 in a way that takes the whole value
  (`ulTaskNotifyTake`, stream and message buffers, `pthread_join`).

### `devkit_compression` (`libs/devkit_compression`)

Streaming inflate and LZW; see its README.

### `devkit_crypto` (`libs/devkit_crypto`)

Cryptography on mbedTLS's PSA Crypto and its own code: hashes, MACs, KDFs,
AEADs, key agreement, signatures, Ed25519, X.509, a TLS 1.3 client and the CA
bundle; see its README.

### `devkit_gfx` (`libs/devkit_gfx`)

2D rendering, IDF-independent and host-testable; see its README.

### `devkit_ui` (`libs/devkit_ui`)

UI framework on `bsp_rs`; see its README.

### `media_engine` (`libs/media_engine`)

The chip's media accelerators (the 2D engine for now) on IDF's HAL and 2D-DMA
allocator; see its README.

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
