# Simulator host

The debug host of a `sim` target (`devkit run simulator`, docs/devkit-cli.md)
runs the simulator firmware as a child process, shows its panel in a window
with controls beside it, and feeds it input. The firmware
process only sees a PSRAM region and an event socket, so it could later be
replaced by an emulator without changing the host.

- The window shows the panel inside the device's outline (`tools/devkit/assets`,
  SVG in panel-pixel units), fitted to the window or at a fixed 25–100 %, with
  the controls and the firmware log on the right.
- `--headless` opens no window; the host still supervises the firmware and
  serves clients.
- `--psram-mb` sizes the PSRAM region (default 32).
- Firmware stdout and stderr, merged in order like a device console, go to the
  host's log: the terminal, the log pane and attached clients.

Only firmware using `libs/bsp_rs` draws through the host. The C `bsp` keeps
its in-process SDL window.

## Firmware process

The host starts the firmware with:

| variable | meaning |
|---|---|
| `SIM_HOST_FD` | inherited Unix stream socket for events |
| `SIM_PSRAM_PATH` | file to map as the PSRAM region |
| `SIM_PSRAM_SIZE` | its size in bytes |

- idf_compat maps the region and serves `MALLOC_CAP_SPIRAM` from it, so
  framebuffers land where the host can read them. Without the variables
  SPIRAM comes from the host heap.
- `libs/sim_device` owns the socket (`sim_device::host()`, `None` without a
  host). The firmware exits when the host closes it.
- `esp_restart()` exits with status 75; the host starts the firmware again.
  Any other exit leaves it stopped until Reset or Power on.

## Events

Fixed 64-byte records, defined once in `libs/sim_device/src/protocol.rs` and
compiled into the firmware and `tools/devkit`. Byte 0 is the tag, fields follow little-endian.

| event | direction | fields |
|---|---|---|
| `Hello` | firmware → host | protocol version; the harness counts the firmware as up from here |
| `Device` | firmware → host | board name; the window title, and picks the device outline |
| `Present` | firmware → host | offset into the PSRAM region, width, height, stride, pixel format |
| `Brightness` | firmware → host | backlight 0..=100 |
| `Touch` | host → firmware | every contact currently down, panel pixels |

The host reads the presented frame straight from the region each refresh, so a
frame drawn while shown tears as it would on the panel.

## Harness

Script lines go to the host (docs/devkit-cli.md). For firmware that presents
through the host, it takes touches and captures itself from the shared frame;
`reset` restarts the firmware process. Everything else reaches the firmware's
harness on its stdin.
