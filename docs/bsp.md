# bsp_rs

The board support package in Rust (`libs/bsp_rs`). The board is chosen with
`CONFIG_BSP_RS_BOARD_*`, independent of the C `bsp`'s `CONFIG_BSP_BOARD_*`.
`firmware/tab5_rs` builds it for the Tab5 and its simulator.

```rust
let board = bsp_rs::init(&bsp_rs::Config::default())?;
```

- `init` brings the board up once; later calls return `INVALID_STATE`.
- `Board` holds a handle per capability: `&'static T` for handles shared
  between tasks, owned values where the caller holds the state.
- Handle types are per-board aliases. A capability the board lacks is
  `Absent`, an empty enum, so code using it compiles away.

## Display

`board.display` is shared by every task (`&'static Display`); its methods take
`&self` and never lock.

- `frame_buffer(i)` returns a `FrameBuffer`: a raw, `Copy`able view of a
  host framebuffer, sendable to other tasks. Writing through `as_mut_slice` /
  `row_mut` is `unsafe`: tasks drawing to overlapping areas at the same time
  is the caller's to prevent. DMA engines take `as_ptr()`.
- `present(i)` puts framebuffer `i` on the panel at the next frame boundary
  and returns without waiting for it. `wait_frame()` blocks until that boundary
  and `scanning()` is the framebuffer being scanned, so a framebuffer other than
  it and the last presented one is free to draw into.
- `blitter(n)` returns a `Blitter` for the calling task, on chips with a 2D
  engine and in the simulator. `start` queues packed rows for an area of the
  logical picture, rotated into a framebuffer, with up to `n` in flight.
  Rotated copies must span at least 8 rows of the area.
- `set_brightness` takes 0..=100; the backlight starts off.
- `Rect`, `Size`, `Rotation` and `ImageFormat` are `devkit_gfx`'s. `init`
  and the 2D engine return `NOT_SUPPORTED` for a format they cannot take; the
  2D engine returns `INVALID_ARG` for an empty rectangle or one left of or
  above the origin.
- `FrameBuffer::target(rotation, area)` is a `devkit_gfx::Target` over an area
  of the logical picture, for writers that take one. It is `unsafe` for the
  same reason as `as_mut_slice`.
- `readback(area)` lends what the panel shows, in its pixel format, as `Rows`
  from `next()`. It is `unsafe`: rows may be torn by concurrent drawing, and
  only one `Readback` may exist at a time.

## Touch

`board.touch` is shared the same way. `read` returns the latest sample without
waiting on the dispatch task, `set_event_cb` delivers each sample on that task,
and `wait_interrupt` blocks until the controller signals. `inject` delivers
points as if the controller had reported them, without waking `wait_interrupt`.

## Power

`board.power` is on every board, shared the same way.

- `read` returns a `PowerState`: `source` is what the board runs on and
  `battery` is `None` without one. A field the board cannot measure is `None`;
  `level` is always there, a linear map of the voltage on most boards.
- `off` cuts the power and returns only when it stays on, as external power
  holds some boards up; the audio output is back as it was then. `restart` is
  a soft reboot. Both silence the audio output first.
- `set_charge` and `set_switch` return `NOT_SUPPORTED` for what the board lacks.

## RTC

`board.rtc` is a battery-backed clock in local wall-clock time, shared the same
way; `None` on a board without one.

- `now` and `set` take a `DateTime`; the weekday comes from the date, and
  `seconds` / `from_seconds` convert to seconds since 1970 on the same clock.
  `set` refuses a date that does not exist or that the chip cannot hold.
- `is_valid` is `false` once the clock lost power, until the next `set`.
- `start_timer(period)` raises the chip's interrupt and expiry flag every
  `period` until `stop_timer`; `timer_expired` and `clear_timer` read and clear
  the flag, which survives deep sleep. A board may refuse short periods.

## SD

`board.sd` is the card slot, shared the same way; `None` on a board without
one.

- `mount(path, config)` mounts the card's FAT file system at `path`, which
  `idf::fs` then reaches; `unmount` and `is_mounted` follow. `MountConfig`
  defaults to 5 open files at the board's speed.
- On boards whose SDMMC controller also serves another slot, the board owns the
  controller and the card is one of its users.
- The simulator maps the mount point onto a host directory:
  `SIMULATOR_SDCARD_PATH`, else `simulator/sdcard` under the working directory.

## IMU

`board.imu` is shared the same way, in panel axes: +X right, +Y down, +Z into
the screen.

- `read` returns the newest `Sample`: accel in g, +1 pointing up at rest, and
  gyro in dps. Its timestamp stays put until the chip has new data.
- `configure` takes a `Request`, where `None` keeps a value, and returns what the
  chip settled on, rounded up to what it supports.
- `set_orientation_enabled(true)` tracks the pose on the dispatch task every
  100 ms. A pose must hold for 300 ms before it counts, and free fall or a shock
  keeps the last one. `orientation()` is `None` until there is one.
- `Orientation::Rotated(r)` is the `Rotation` that shows the picture upright,
  ready for `Blitter::start`. `set_orientation_cb` delivers each new pose on the
  dispatch task.
- `inject` replaces the sensor, in panel axes, until `None`.

## Wi-Fi and BLE

A board whose radios sit on an esp-hosted coprocessor (the Tab5's C6) serves
them through the IDF APIs, not through `Board`: with
`CONFIG_ESP_WIFI_REMOTE_LIBRARY_CUSTOM`, `esp_wifi_init()` resets the
coprocessor, brings the SDIO link up and the rest of `esp_wifi` maps to its
RPCs; with NimBLE as a host-only stack, `nimble_port_init()` brings the link
and the coprocessor's controller up. `init` only records where the coprocessor
is.

- The firmware brings in `esp_wifi` (through `libs/wifi`, for one) and `bt`
  itself; `bsp_rs` does not require them.
- The link is `libs/esp_hosted_enhanced`'s host in Rust (`driver/esp_hosted`),
  with its wire format, priorities and SDIO handling, and works with the stock
  coprocessor firmware too. A firmware with `bsp_rs` gets it instead of the C
  host.
- On the Tab5 it shares the SDMMC controller with the card slot.
- `firmware/esp_hosted_enhanced/rust_check` runs the Wi-Fi and BLE benches on it.
