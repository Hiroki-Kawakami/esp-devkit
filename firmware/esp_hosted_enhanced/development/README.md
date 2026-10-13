# esp_hosted_enhanced development firmware

M5Stack Tab5 firmware for developing the hosted Wi-Fi and BLE stack. It joins
the network saved by `libs/wifi`, serves `netbench.py`, runs one side of
`blebench.py` and flashes the coprocessor for `c6flash.py`. The host is `libs/esp_hosted_enhanced` unless `stock` selects
the esp-hosted component; each board/host pair builds in `esp32p4/build_<board>_<host>`.

```sh
nix develop -c ./run.sh -p /dev/cu.usbmodemXXXX flash               # on-board C6
nix develop -c ./run.sh xiao -p /dev/cu.usbmodemXXXX flash          # XIAO ESP32C6 on the M-Bus
nix develop -c ./run.sh tab5 stock -p /dev/cu.usbmodemXXXX flash    # stock esp-hosted host
nix develop -c python3 netbench.py --port /dev/cu.usbmodemXXXX --json result.json
nix develop -c python3 c6flash.py --port /dev/cu.usbmodemXXXX image.bin
nix develop -c python3 sdiobench.py --port /dev/cu.usbmodemXXXX --json result.json
nix develop -c python3 blebench.py --periph /dev/cu.usbmodemXXXX --central /dev/cu.usbmodemYYYY
```

`netbench.py` reads the board's IP and heap figures over the harness console
and measures from the PC, which must be able to reach the board's network. The
board listens on TCP 5001 (sink), TCP 5002 (source) and UDP 5003 (echo).

`sdiobench.py` measures the SDIO link alone over the test channel: throughput
both ways, echo round trips, and host->coprocessor with a slow consumer (each
packet held 1 ms) to exercise the backpressure, and a transfer while IDF sdmmc
commands go through the shared-controller hook. `--c6-log` forwards the
coprocessor log. Both need the esp_hosted_enhanced host and coprocessor.

`blebench.py` needs two boards with this firmware, any host/coprocessor pair
on either: the peripheral advertises a bench service, the central connects
(2M PHY, data length extension) and streams writes, then the peripheral
streams notifications. It also measures each side's HCI command round trip
and the scan report rate. `--load up|down|both` keeps the central's SDIO link
busy over the test channel during its measurements.

`c6flash.py` streams an image through the esp-hosted OTA requests, so it works
with the stock coprocessor firmware too. `--recovery` restarts the host with
the coprocessor's IO2 held low to flash from its safe mode (see
`../coprocessor/README.md`).

The XIAO wiring has no EN line: pass `--reset-cmd` with a command that resets
it over its own USB, e.g.
`--reset-cmd "python -m esptool -p /dev/cu.usbmodemYYYY --after hard-reset read-mac"`.

| signal | Tab5 M-Bus | XIAO |
|---|---|---|
| CLK / CMD | G16 / G17 | D8 (GPIO19) / D10 (GPIO18) |
| D0 / D1 / D2 / D3 | G2 / G3 / G4 / G45 | D9 / D3 / D4 / D5 (GPIO20-23) |
| IO2 | G51 | D2 (GPIO2) |

The partition table keeps `nvs` at 0x9000 so credentials saved by other Tab5
firmware with the same layout are picked up.
