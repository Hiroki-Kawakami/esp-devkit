# esp_hosted_enhanced

esp-hosted 2.12 compatible Wi-Fi remote over SDIO: the shared wire format
(`inc/hosted_wire.h`, the vendored `esp_hosted_rpc` protobuf and protobuf-c
runtime) and, on a host without native Wi-Fi, an `esp_wifi_remote`
implementation. It talks to the stock esp-hosted coprocessor firmware as well
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

The host issues its SDIO commands directly on the SDMMC controller and polls
them. Anything else on the same controller (an SD card on the other slot) uses
`hosted_host_sdmmc_init` and `hosted_host_sdmmc_do_transaction` as its
`sdmmc_host_t` hooks, and brackets its mount and unmount with
`hosted_host_sdmmc_acquire/release`.
