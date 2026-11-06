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
- The window's Rotation turns the simulated device clockwise: the IMU reports
  that resting pose, so orientation tracking follows it. `--headless` stays
  upright; the `imu` script command overrides it.
- The RTC runs on the host's local clock with the offset the firmware last set,
  kept across restarts. `SIMULATOR_RTC_INVALID=1` in the host's environment
  starts it invalid. It has no timer.
- Power sets the battery level, external power and whether a battery is in.
  The firmware reads them through `Power::read`; they survive restarts. A
  firmware calling `Power::off` exits, and Power on starts it again.
- Audio plays on the default output device, resampled to its rate. With
  `--headless`, or when no device opens, the host keeps the same time without
  sound, so firmware pacing does not change.
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
| `SIM_RTC` | the clock to start with: local wall-clock seconds since 1970, then 1 or 0 for valid |
| `SIM_POWER` | the power state to start with: `<level> <external> <battery>`, flags as 0 or 1 |
| `SIM_NET_FD` | inherited Unix datagram socket, one Ethernet frame per datagram |
| `SIM_USB_FD` | inherited Unix datagram socket, one plugged-in USB device per datagram |

- idf_compat maps the region and serves `MALLOC_CAP_SPIRAM` from it, so
  framebuffers land where the host can read them. Without the variables
  SPIRAM comes from the host heap.
- `libs/bsp_rs/src/sim` owns the socket (`None` from `sim::host()` without a
  host) and the simulated panel, touch and audio. The firmware exits when the
  host closes it.
- `esp_restart()` exits with status 75; the host starts the firmware again.
  Any other exit leaves it stopped until Reset or Power on.

## Network

devkit_net registers a Wi-Fi radio in the simulator that joins any network at
once; its station sends and receives frames on `SIM_NET_FD`. The host runs
libslirp behind the socket, as QEMU's user networking does:

- DHCP gives 10.0.2.15/24, gateway 10.0.2.2, DNS 10.0.2.3.
- 10.0.2.2 stands for the host's 127.0.0.1, so the firmware reaches servers
  on the host there; 10.0.2.3 forwards to the host's resolver. Other
  addresses go out through the host's own sockets, ICMP echo included.
- Nothing connects in from the host. Without the variable, connecting fails.

## USB host

usb_host_rs's controller in the simulator is a USB/IP client. Each datagram on
`SIM_USB_FD` is a bus id with a connection attached (`SCM_RIGHTS`); the
firmware imports that bus id over it, and the device is plugged in until the
connection closes. The host connects and hands over:

- `usb attach usb <vid:pid> [serial]`: a device plugged into this computer,
  which the host serves itself over nusb; `usb list` shows them. Attaching
  again unplugs the device before.
- `usb attach tcp <host:port> [busid]`: a USB/IP server such as a simulated
  usb_device (`firmware/usbd_loopback`) or Linux `usbipd`; bus id `1-1` by
  default.
- `usb detach` unplugs it; so does a firmware restart, and so does pulling
  the device out of this computer.

Serving a device of this computer:

- An interface is claimed on the first transfer to one of its endpoints, so
  the others stay with whoever has them. On macOS an interface a kernel
  driver holds (mass storage, audio, video, HID) cannot be claimed, and an
  `adb` server holds a phone's ADB interface until `adb kill-server`.
- SET_CONFIGURATION, SET_INTERFACE and CLEAR_FEATURE(ENDPOINT_HALT) go
  through the OS; SET_CONFIGURATION to the active configuration does
  nothing. Isochronous transfers fail with a stall.
- An unlink cancels every transfer on that endpoint, since that is all the OS
  offers: the others come back with what they moved, or are queued again in
  order when they moved nothing.

The firmware completes SET_ADDRESS itself, since the server has the device
addressed already. Without the variable no device ever appears.


Fixed 64-byte records, defined once in `libs/bsp_rs/src/sim/protocol.rs` and
compiled into the firmware and `tools/devkit`. Byte 0 is the tag, fields follow little-endian.

| event | direction | fields |
|---|---|---|
| `Hello` | firmware → host | protocol version; the harness counts the firmware as up from here |
| `Device` | firmware → host | board name; the window title, and picks the device outline |
| `Present` | firmware → host | offset into the PSRAM region, width, height, stride, pixel format |
| `Brightness` | firmware → host | backlight 0..=100 |
| `Touch` | host → firmware | every contact currently down, panel pixels |
| `AudioOpen` | firmware → host | stream id, offset and size of a PCM ring in the PSRAM region, sample rate, sample bytes (2 or 4), channels |
| `AudioWrite` | firmware → host | stream id, bytes written so far |
| `AudioClose` | firmware → host | stream id |
| `AudioMute` | firmware → host | play as silence, keeping time; outlives streams |
| `AudioPlayed` | host → firmware | stream id, bytes played so far; the firmware's writes pace on it |
| `Rtc` | firmware → host | the clock was set: local wall-clock seconds since 1970, valid |
| `Power` | host → firmware | battery level 0..=100, external power, battery in; sent at `Hello` and on every change |
| `Imu` | host → firmware | accel and gyro in panel axes; sent at `Hello` and when the window's rotation changes |

The host reads the presented frame straight from the region each refresh, so a
frame drawn while shown tears as it would on the panel. Audio works the same
way: the host reads samples from the ring, and the firmware only announces how
far it has written. The simulated audio has no hardware gain and no headphone
jack, so `DspMode::Disable` has no volume there.

## Harness

Script lines go to the host (docs/devkit-cli.md). For firmware that presents
through the host, it takes touches and captures itself from the shared frame;
`reset` restarts the firmware process. Everything else reaches the firmware's
harness on its stdin.
