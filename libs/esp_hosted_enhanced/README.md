# esp_hosted_enhanced

esp-hosted 2.12 compatible Wi-Fi and BLE remote over SDIO: the shared wire
format (`inc/hosted_wire.h` and the vendored `esp_hosted_rpc` schema, compiled
to C at build time by `tools/protogen`) and, on a host without native radios,
an `esp_wifi_remote` implementation and a NimBLE host transport. It talks to the stock esp-hosted coprocessor firmware as well
as to `firmware/esp_hosted_enhanced/coprocessor`.

## Host

Select `CONFIG_ESP_WIFI_REMOTE_LIBRARY_CUSTOM`; `CONFIG_ESP_HOSTED_ENHANCED_HOST`
follows and the stock esp-hosted component builds nothing. Then set the SDIO
slot, pins and clock under *esp_hosted_enhanced*:

```
CONFIG_ESP_WIFI_REMOTE_LIBRARY_CUSTOM=y
CONFIG_ESP_HOSTED_ENHANCED_PIN_CLK=12
CONFIG_ESP_HOSTED_ENHANCED_PIN_CMD=13
CONFIG_ESP_HOSTED_ENHANCED_PIN_D0=11
CONFIG_ESP_HOSTED_ENHANCED_PIN_D1=10
CONFIG_ESP_HOSTED_ENHANCED_PIN_D2=9
CONFIG_ESP_HOSTED_ENHANCED_PIN_D3=8
CONFIG_ESP_HOSTED_ENHANCED_PIN_RESET=15
```

`esp_wifi_init()` resets the coprocessor and brings the link up; the rest of
the `esp_wifi` API maps to its RPCs. `inc/hosted_host.h` covers what lies
beside that API: connecting without Wi-Fi, the firmware version, OTA, and the
coprocessor's extensions (log forwarding, the test channel) when it advertises
them.

Only the station interface is implemented; other `esp_wifi` calls return
`ESP_ERR_NOT_SUPPORTED`.

## BLE

With NimBLE as a host-only stack, `CONFIG_ESP_HOSTED_ENHANCED_BT` follows and
NimBLE talks HCI to the coprocessor's controller:

```
CONFIG_BT_ENABLED=y
CONFIG_BT_NIMBLE_ENABLED=y
CONFIG_BT_CONTROLLER_DISABLED=y
CONFIG_BT_NIMBLE_TRANSPORT_UART=n
```

`nimble_port_init()` brings the link up if Wi-Fi has not, then initializes and
enables the controller; `nimble_port_deinit()` disables and deinitializes it.
Nothing else is needed beside the NimBLE API. Coprocessor firmware 1.x enables
its controller at boot and is used as it is.

When NimBLE runs out of ACL buffers, received ACL data waits for one (up to
100 ms) and stalls the link meanwhile instead of being dropped.

## Link

RPC, HCI and the link's own events have their own transmit buffers and go out
ahead of Wi-Fi and test data, which also leaves the last two receive buffers
on the coprocessor to them, so a saturated data path does not hold them back.

The host issues its SDIO commands directly on the SDMMC controller. Short ones
are polled; transfers of 4 KB and more wait for the completion interrupt. Anything else on the same controller (an SD card on the other slot) uses
`hosted_host_sdmmc_init` and `hosted_host_sdmmc_do_transaction` as its
`sdmmc_host_t` hooks, and brackets its mount and unmount with
`hosted_host_sdmmc_acquire/release`.

## Tests

`test/` checks the generated RPC code against libprotobuf-c on the PC (from the
repository root):

```
nix develop -c sh -c 'cmake -S libs/esp_hosted_enhanced/test -B build/hosted_rpc_test -G Ninja && cmake --build build/hosted_rpc_test'
build/hosted_rpc_test/hosted_rpc_test
```
