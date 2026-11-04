# ESP-IDF dependencies

What devkit_net relies on in ESP-IDF beyond its public API, and what to check
when moving to another IDF version. Written against IDF 6.1.

## Built-in Wi-Fi (`src/wifi_native.rs`, `src/wifi_native.c`)

The station runs on the Wi-Fi driver's netstack hooks, the same ones esp_netif
uses, without esp_netif. All of these are declared in `esp_private/wifi.h`, which
IDF does not keep stable across versions:

| function | used for | relies on |
|---|---|---|
| `esp_wifi_internal_reg_rxcb` | received frames | called on the Wi-Fi task; the buffer stays the driver's until `esp_wifi_internal_free_rx_buffer` |
| `esp_wifi_internal_free_rx_buffer` | returning a received buffer | may be called from any task, after the stack processed the frame |
| `esp_wifi_internal_tx` | sending | copies the frame before returning; `ESP_ERR_NO_MEM` while all `ESP_WIFI_DYNAMIC_TX_BUFFER_NUM` buffers are in use, which the link reports as `Busy` |
| `esp_wifi_set_tx_done_cb` | `LinkRx::tx_ready` | one callback slot for the whole driver, so another user of it would replace ours; refused before `esp_wifi_start`; called on the Wi-Fi task for every frame sent |

On an upgrade, check that these keep their signatures and the behaviour in the
"relies on" column, `wifi_tx_done_cb_t` in particular. A received buffer that is
never freed starves the driver of receive buffers; a tx-done callback that
stops being called only costs the 1 ms retry.

Other details that follow IDF's types:

- `WIFI_INIT_CONFIG_DEFAULT()` is a macro, so `devkit_net_wifi_init_default` in
  C expands it. The init config sets `nvs_enable = 0` and the storage to RAM;
  `idf::nvs::init()` still runs so that RF calibration data is kept.
- bindgen lays out the `he_*` bitfields of `wifi_sta_config_t` wrong, so the
  station config is filled in C (`devkit_net_wifi_set_sta`), and
  `wifi_config_t`, `wifi_sta_config_t`, `wifi_event_dpp_config_received_t` and
  `esp_wifi_set_config`/`get_config` are blocklisted from bindgen
  (`CMakeLists.txt`). Recheck on an upgrade whether the layout is right and the
  workaround can go.
- The station asks for WPA2-PSK or better when a password is given, PMF
  capable, SAE with both hunting-and-pecking and H2E.
- The default event loop is created if nobody did (`ESP_ERR_INVALID_STATE`
  means it exists); the driver's events arrive on its task.
- IDF does not allow a component's `REQUIRES` to depend on Kconfig, so
  `esp_wifi` is a dependency on every device build, with Wi-Fi or without.

## Keeping lwIP out of the link

devkit_net does not use lwIP or esp_netif, but other components can pull them
in. With `CONFIG_ESP_WIFI_ENTERPRISE_SUPPORT=y` (IDF's default), wpa_supplicant
links EAP, which links mbedTLS's x509, which calls `lwip_inet_pton`, which brings
lwIP's socket layer and core. A firmware on devkit_net sets
`CONFIG_ESP_WIFI_ENTERPRISE_SUPPORT=n` in its own `sdkconfig.defaults`
(`firmware/rust_hello/esp32s3`): a component's Kconfig cannot force another
component's option off. Using mbedTLS's x509 for TLS brings the same reference
back, and needs its own answer.

To find what pulls lwIP in, read the "Archive member included to satisfy
reference" section of the firmware's map file, which names the object and
symbol behind every archive member linked.

## IRAM placement

`DEVKIT_NET_IRAM_OPT` puts functions in sections named `.iram1.devkit_net`,
which IDF's linker script places in IRAM through its `.iram1.*` pattern. Check
that pattern on an upgrade; if it goes, the functions land in flash silently
(`nm` shows them at a `0x42...` address instead of `0x403...`). Literal pools
come along with them. Functions marked `#[cold]` and `#[inline(never)]` stay in
flash on purpose; marking more of them is how the IRAM size is kept down.

## Kconfig

A new devkit_net Kconfig symbol reaches an existing `sdkconfig` only after
`idf.py reconfigure`; until then a build does not see it, even with the symbol in
`sdkconfig.defaults`.

## Elsewhere

- The Wi-Fi coprocessor (esp_hosted) driver is in `libs/bsp_rs/src/driver/esp_hosted`
  and registers itself with `register_wifi`; its IDF dependencies are its own.
- The simulator's network needs libslirp in the devkit host
  (`tools/devkit/src/host/net.rs`, `flake.nix`), not in the firmware.
