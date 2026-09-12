# Test harness

Scripted UI verification — drive the UI and read the screen back, with the same
script running against the host simulator or a real board.

The harness exists so an agent (or a human) can check a UI change without a
person looking at the panel: tap here, wait for the animation to stop, save a
JPEG of what the screen shows.

## Layers

| piece | what it is |
|---|---|
| `bsp/inc/bsp_harness.h` | BSP hooks: synthetic touch/button injection, panel readback (`CONFIG_BSP_HARNESS`) |
| `libs/harness` | the line protocol and its transport — IDF console on device, stdin/stdout on the host (`CONFIG_HARNESS`) |
| `tools/harness/harness.py` | PC-side driver: runs a script, does the timing, writes captures |

Injected input feeds the same paths real hardware does — a touch goes through
the touch layer's event/snapshot delivery, a button level through the shared
debounce/click state machine — so the app cannot tell a scripted input from a
finger. Timing (`wait`, `settle`, `tap`) lives entirely on the PC side; the
firmware only exposes primitives.

## Quick start

### Simulator

```sh
cmake -S firmware/core_hello/simulator -B firmware/core_hello/simulator/build -G Ninja
cmake --build firmware/core_hello/simulator/build
tools/harness/harness.py --sim firmware/core_hello/simulator/build/simulator \
    --out shots script.txt
```

The simulator always has the harness compiled in. It is launched headless
(`SIMULATOR_HEADLESS=1`) unless `--window` is passed, and exits when the script
ends.

### Board

```sh
tools/harness/harness.py --port /dev/cu.usbserial-XXXX --baud 921600 \
    --out shots script.txt
```

`--port` is the console port and `--baud` must match
`CONFIG_ESP_CONSOLE_UART_BAUDRATE` (default 115200; ignored for
USB-Serial-JTAG). **Close `idf.py monitor` first** — the harness shares the
console port with the log stream. `--log` echoes the target's log lines to
stderr while the script runs.

The script argument defaults to `-` (stdin), so one-off commands can be piped:

```sh
echo "settle
capture home.jpg" | tools/harness/harness.py --port /dev/cu.usbserial-XXXX --out shots
```

`harness.py` exits non-zero when a command fails or the target stops answering.
The serial path needs `pyserial` (present in the ESP-IDF python env).

## Script language

One command per line, `#` starts a comment, blank lines are ignored.

| command | effect |
|---|---|
| `wait <ms>` | sleep |
| `settle [<max_ms>]` | poll until the UI reports idle (no LVGL animation), default budget 5000 ms |
| `capture <path> [quality]` | JPEG of the whole panel, `path` is relative to `--out`, `quality` 0 = encoder default |
| `tap <x> <y> [id]` | press and release |
| `down <x> <y> [id]` | press, or move a held contact |
| `move <x> <y> [id]` | same as `down`, for readability |
| `up [id]` | release |
| `btn <id> [click\|down\|up]` | physical button in the `bsp_button` id space, default `click` |
| `quit` | stop the script (implicit at end of file) |
| anything else | sent verbatim — app commands registered with `harness_register()` |

`id` is the touch contact (default 0); use 1, 2, … for multi-touch gestures.
Coordinates are **panel pixels** — the same space `capture` returns, whatever
rotation the app's `DisplayManager` viewport uses.

`settle` needs three consecutive idle answers 50 ms apart; on timeout it warns
on stderr and the script continues. `tap` and `btn click` hold for 80 ms.

```
# open the list screen and shoot it
settle
tap 160 200
settle 3000
capture list.jpg
btn 0
settle
capture after-button.jpg 90
```

## Enabling it on a board

Set `CONFIG_HARNESS=y` (menuconfig: *Test harness → Scripted UI test harness*),
either in `sdkconfig.defaults` or via menuconfig. It compiles the console
transport, selects the `bsp_harness.h` hooks and pulls in the JPEG encoder from
`libs/image_framework`. Leave it off in production builds.

Nothing is needed in the app: `lvgl_port_init()` is wrapped (`-Wl,--wrap`, added
only for `CONFIG_HARNESS` builds) and starts the harness after the real init,
registering the LVGL idle predicate and the lock that keeps a capture from
racing a flush. Firmware **without** an LVGL port calls `harness_start()` itself
once the BSP is up.

The console must be UART or USB-Serial-JTAG (the build errors out otherwise).
Large captures go much faster over USB-Serial-JTAG; on a UART console raise the
baud rate:

```
CONFIG_HARNESS=y
CONFIG_ESP_CONSOLE_UART_BAUDRATE=921600
```

The harness adds the JPEG encoder to the image, so a firmware near its partition
limit may need a bigger app partition (`firmware/tab5_hello/esp32p4` switched to
a custom `partitions.csv` for this).

## What each target supports

`info` reports panel size, pixel format, whether touch is present, the button
count and whether `capture` works, so a script can check before it shoots.

Capture needs a panel whose contents can be read back
(`BSP_DISPLAY_CAP_READBACK`):

| target | capture source |
|---|---|
| simulator (any board) | the presented image |
| Tab5 (MIPI-DSI) | last flushed framebuffer |
| S31 Korvo (RGB LCD) | framebuffer |
| Paper (IT8951E) | controller GRAM shadow, grayscale |
| Paper S3 / LilyGo T5 (epd_ll) | the drawn target gray — matches the glass once `wait_idle` returns |
| AirQ (GDEY0154D67) | 1 bpp framebuffer, expanded to gray |
| Core / Core2 (ILI9342C) | `RAMRD` over SPI — see below |
| CoreS3 (ILI9342C) | none: MISO is not wired on that board |
| StickC-Plus (ST7789V2) | none: the panel has no readback, `capture` errors |

On Core/Core2 the panel's SDO is only brought into the SPI bus for
`CONFIG_BSP_HARNESS` builds (a plain build keeps the LCD write-only), reads run
on a second slow-clock device handle, and the microSD sharing that MISO line is
parked deselected at init — otherwise the card answers the panel's reads.

Simulator boards get a stand-in button provider that always samples "released",
so its only input is `btn`, but the `bsp_button` id space matches the device.

## App-specific commands

Any component can add commands; unknown script lines are passed through
verbatim, so `myapp-screen list` reaches a handler registered as `myapp-screen`.

```c
#include "harness.h"

static bool cmd_screen(int argc, const char *const *argv, void *user) {
    if (argc < 2) {
        harness_reply("ERR %s: need a screen name", argv[0]);
        return true;
    }
    if (!app_show_screen(argv[1])) return false;
    return true;
}

harness_register("myapp-screen", cmd_screen, NULL);
```

Return `true` when handled — the core sends `#OK <name>` unless the handler
already replied — and `false` for `#ERR`. Handlers run on the harness transport
thread, so take the LVGL lock before touching widgets. Limits: 32 commands,
8 arguments, 512-byte command lines, ~250-byte replies.

`libs/wifi`'s simulator backend is a working example (`wifi-aps`,
`wifi-connect-result`, `wifi-delay`, `wifi-drop`), letting a script steer fake
Wi-Fi results while driving the UI.

## Line protocol

Only needed to write a driver other than `harness.py`. Commands are one line
each; every reply is a single `#`-prefixed line, so a client can pick replies
out of the log stream sharing the channel.

```
ping                    -> #OK ping
info                    -> #OK info <w> <h> <pixfmt> <touch> <buttons> <capture>
down <id> <x> <y>       -> #OK down      press / drag contact <id> (panel px)
move <id> <x> <y>       -> #OK move
up <id>                 -> #OK up
btn <id> down|up        -> #OK btn
idle                    -> #OK idle 0|1
snap [quality]          -> #OK snap <w> <h>, then #D <base64> lines, #END <bytes>
quit                    -> #OK quit
```

`snap` streams a JPEG of the whole panel, encoded one panel row at a time (peak
memory is one row plus the encoder's working set, so it fits boards without
PSRAM). On device, replies are written under a mutex so a concurrent log line
cannot split one. On the host, protocol lines go to stdout and logs to stderr,
and EOF on a piped stdin ends the run like `quit`.

## Simulator notes

- `SIMULATOR_HEADLESS=1` skips window/renderer creation; `harness.py` sets it
  unless `--window` is given.
- With a window, the mouse drives contact id 0 — the same slot `tap` uses by
  default. Use ids ≥ 1 for synthetic contacts you want to keep independent.
- The `s` key saves a timestamped screenshot under `screenshots/`, through the
  same capture path as `snap`.
- `libs/harness/src/harness.c` also compiles standalone into host unit tests
  that only need `harness_register` (see `libs/wifi/test/run.sh`).

## Troubleshooting

| symptom | cause |
|---|---|
| `harness did not answer ping` | `idf.py monitor` (or another client) holds the port; wrong `--baud`; `CONFIG_HARNESS` not set in the flashed build |
| `snap: no readback on this panel` | the board has no readback — see the table above |
| `capture` is very slow | UART console at 115200; raise `CONFIG_ESP_CONSOLE_UART_BAUDRATE` or use USB-Serial-JTAG |
| `timeout waiting for reply` | the UI task is blocked, or a capture is still streaming (the snap timeout is 30 s) |
| taps land in the wrong place | coordinates must be panel pixels, not LVGL logical coordinates of a rotated viewport |
