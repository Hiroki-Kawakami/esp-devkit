# devkit CLI

`devkit` builds, flashes and runs a firmware project's targets, and talks to
the firmware while it runs. It is on `PATH` in the dev shell (`nix develop`),
built from `tools/devkit` as the flake's `devkit` package.

```sh
cd firmware/tab5_rs
devkit run              # default target
devkit run simulator
devkit run esp32p4 --port /dev/cu.usbmodem1101
```

## Projects and targets

A project is the directory holding a `devkit.toml`; `devkit` looks for the
nearest one at or above the working directory (or `-C <dir>`).

```toml
default = "esp32p4"

[targets.esp32p4]
kind = "idf"                # an ESP-IDF project in ./esp32p4
flash-baud = 3000000        # optional; also monitor-baud, dir

[targets.simulator]
kind = "sim"                # a devkit_simulator() build in ./simulator
window = false              # the firmware draws its own window (C bsp)
headless = true             # no window at all, as --headless

[targets.test]
kind = "sim"
dir = "simulator"
build = "rust_test"         # build this CMake target only; nothing runs

[targets.pcapp]
kind = "cmd"                # shell commands, run in the project directory
build = "cargo build --release --manifest-path pc/Cargo.toml"
run = "pc/target/release/pcapp"
```

`dir` defaults to the target's name for `idf` and `sim`.

## Commands

| command | does |
|---|---|
| `targets` | list the project's targets |
| `build [target]` | `idf.py build`, the simulator's CMake build, or `build` |
| `flash [target] [--port P]` | `idf.py flash`, borrowing the port from a host that holds it |
| `run [target]` | build, then: `idf` flashes and opens the board's console; `sim` starts the simulator; `cmd` runs `run` |
| `monitor [target] [--port P] [--backlog [N]]` | log and commands without building or flashing: attaches to the target's running host, else opens the board's console; `--backlog` first prints the last `N` log lines (all when `N` is left out) |
| `menuconfig [target]` | edit the target's sdkconfig (`idf` and `sim`) |
| `save-defconfig [target]` | write the differences from the defaults to its `sdkconfig.defaults` |
| `reconfigure [target]` | regenerate its sdkconfig and build files, as needed after changing a `Kconfig` |
| `idf [target] <args>` | `idf.py` in the target's directory (`menuconfig`, `fullclean`, ...) |
| `devices` | list connected serial devices: port, USB id, serial number, name, product, the host holding it |
| `devices name <name> <device>` | name a device, for every project (`~/.config/esp-devkit/devices.toml`) |
| `devices unname <name>` | forget a name |
| `use [target] <device>` | pin a target to a device in `devkit.local.toml` |
| `use [target] --clear` | remove the pin; `use` alone lists the pins |
| `hosts` | list running hosts |
| `cmd [-t H] [--log] <line>...` | run harness lines, print each reply, stop at the first error |
| `script [-t H] [--log] <file\|->` | the same for a script file or stdin |
| `resgen <inputs\|index\|entry> <definition> <fonts dir> <out> [entry]` | build step behind `devkit_rust_crate(RESOURCES ...)`, see docs/resgen.md |

`run` takes `--headless` (no window) and `--psram-mb N` (simulator PSRAM,
default 32); arguments after `--` go to the simulator firmware.

## Devices

A device is given by port path, USB serial number or name, anywhere one is
expected. The port for a target comes from `--port`, else `ESPPORT`, else its
pin, else the only USB serial device connected.

`devkit.local.toml` sits next to `devkit.toml`, is not tracked, and holds a
target's pin without touching tracked files. It stores the serial number when
the device has one, so the pin follows the board across USB ports, the port
path otherwise, or the name when pinned by name:

```sh
devkit devices name tab5-st7121 /dev/cu.usbmodem114201
devkit use esp32p4 tab5-st7121
devkit run                        # finds that board wherever it is plugged in
```

Add `devkit.local.toml` to `.gitignore` in repositories that use esp-devkit.

## Scripted runs

`devkit run [target] --script <file|->` builds the target, starts the host,
waits for the firmware to come up, runs the script, stops, and exits non-zero
when a line fails, the firmware exits, or it never comes up:

```sh
devkit -C firmware/core_hello run simulator --script tests/smoke.txt
```

A simulator runs headless unless `--window` is given; `--log` prints the
firmware's log too. On a board the target is flashed and reset first, and the
firmware needs a harness to count as up. `--script` is relative to the
working directory, not to `-C`.

## Hosts

`run` stays in the foreground as the target's *host*: the process that owns
the link to the firmware — the simulator child process, or the board's serial
port, reopened when it drops out across a reset or deep sleep. So does
`monitor` on a board no host holds; unlike `idf.py monitor` it does not reset
the board. The terminal shows the firmware's log, colored by level when stdout
is a terminal, and each line typed is a harness line (below). `quit` ends the
host; at EOF on stdin it keeps serving, so it runs fine in the background.

A host also listens on `$XDG_RUNTIME_DIR/esp-devkit/<pid>.sock` (else
`/tmp/esp-devkit-<uid>/`), so other terminals or tools attach while it runs:

```sh
devkit monitor --backlog 50       # what happened so far, then follow
devkit cmd "tap 160 200" "settle" "capture home.jpg"
devkit script tests/smoke.txt
```

`-t` picks a host by pid or by part of its name (`tab5_rs/simulator`) or port
when more than one runs. A host serializes commands to the firmware; the log
goes to every client that turned it on (`monitor`, `--log`, or the terminal).

## Harness lines

The same lines work in the host's terminal, `cmd`, `script`, and a raw
connection (`nc -U <socket>`). Each gets exactly one reply line, `#OK ...` or
`#ERR ...`; other lines are log.

| line | does |
|---|---|
| `wait <ms>` | sleep |
| `settle [<max_ms>]` | wait until the firmware reports idle three times 50 ms apart (default 5000); `#OK settle 0` on timeout |
| `capture <path> [quality]` | JPEG of the panel; relative paths are against the client's directory |
| `tap <x> <y> [id]` | press and release (80 ms each) |
| `down <x> <y> [id]`, `move ...` | press or drag a contact |
| `up [id]` | release |
| `btn <id> [click\|down\|up]` | physical button, default `click` |
| `imu <ax> <ay> <az> [...]`, `imu rot0\|rot90\|rot180\|rot270\|face-up\|face-down`, `imu release` | IMU override |
| `reset` | reboot (the simulator restarts the process) and wait until the firmware answers |
| `log on\|off` | send the log to this client |
| `backlog [n]` | resend the last `n` log lines |
| `host` | `#OK host <pid> <sim\|serial:PORT> <name>` |
| `quit` | close this client (`run`/`monitor`: end the host) |
| anything else | passed to the firmware's harness (app commands, `idle`, ...) |

Where a line is handled depends on the target, never on the script: on the
simulator the host takes the touch and the capture straight from the shared
frame when the firmware presents through it (`libs/bsp_rs`), and otherwise
everything reaches the firmware's harness (docs/harness.md).

## Decoding

On a board the host reads the stream the way `idf.py monitor` does, for every
client, with or without a terminal:

- Every 8-digit hex address in a line that falls in an executable section of
  the app, bootloader or ROM ELF gets a `--- 0x...: function at file:line` line.
- A panic is buffered and handed to `esp_idf_panic_decoder` through the
  toolchain's gdb, which unwinds the stack dump on RISC-V chips. The stack dump
  is hidden, and the log after it waits for the `--- Backtrace:` result.
- A core dump printed to the console goes to `esp_coredump`; its output replaces
  the base64. One stored in flash is `devkit idf coredump-info`.

These come from esp_idf_monitor; check them after an ESP-IDF update
(`tools/devkit/src/host/decode.rs`):

| marker | esp_idf_monitor source |
|---|---|
| `Core <n> register dump:`, `Stack memory:`, `ELF file SHA256:` | `base/constants.py` (`PANIC_*`) |
| the blank line ending the stack dump, the gdb invocation | `base/serial_handler.py`, `esp_idf_panic_decoder/panic_output_decoder.py` |
| `CORE DUMP START`/`END`, `Press Enter to print core dump to UART...` | `base/coredump.py` |
| `I`/`W`/`E (...)` colors | `base/output_helpers.py` (`AUTO_COLOR_REGEX`) |
| `<target>_rev<n>_rom.elf` in `ESP_ROM_ELF_DIR` | `base/rom_elf_getter.py` |

## Notes

- The firmware's own harness protocol does not depend on the host: the same
  lines typed into `idf.py monitor` work.
- Working on `devkit` itself: `cargo run --manifest-path tools/devkit/Cargo.toml -- <args>`.
