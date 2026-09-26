# esp_hosted_enhanced coprocessor

ESP32-C6 Wi-Fi coprocessor speaking the esp-hosted 2.12 SDIO protocol, so the
stock esp-hosted host can drive it. It reports itself as 2.12.6.

```sh
nix develop -c ./run.sh                       # Tab5 on-board C6 -> build_tab5/
nix develop -c ./run.sh xiao -p PORT flash    # XIAO ESP32C6 over its own USB
```

The Tab5 C6 is only reachable through the host: flash `build_tab5/*.bin` with
`../development/c6flash.py`. The partition table matches the stock one
(`otadata` + `ota_0` + `ota_1`), and the stock bootloader is kept.

## Safe mode

At boot IO2 is sampled with its pull-up. Held low by the host, the firmware
serves only the version and OTA requests. Each such boot is counted in the last
flash sector (used only when no partition covers it); the 3rd in a row switches
to the other OTA slot. Reaching the host clears the count, as does a normal
boot.

## Extensions

Advertised in the INIT event (`HOSTED_TLV_EXT_CAPS`), used only when the host
asks, so a stock host never sees them:

- log channel: the log output is kept in a 4 KB ring from boot and forwarded
  while the host has it enabled (safe mode included)
- test channel: sinks, echoes or generates packets through the same buffers as
  Wi-Fi data, optionally holding each received packet to act as a slow
  consumer
