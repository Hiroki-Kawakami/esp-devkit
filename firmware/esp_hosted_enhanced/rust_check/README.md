# esp_hosted_enhanced Rust host check

M5Stack Tab5 firmware that runs the Rust host in `libs/bsp_rs`
(`driver/esp_hosted`) under the benches of `../development`: `netbench.py` and
`blebench.py` work against it unchanged. The Wi-Fi and BLE benches are
`../development`'s own sources, built against a shim of the C harness API that
forwards to `harness_rs`.

```sh
devkit -C firmware/esp_hosted_enhanced/rust_check run
nix develop -c python3 ../development/netbench.py --port /dev/cu.usbmodemXXXX
nix develop -c python3 ../development/blebench.py --periph /dev/cu.usbmodemXXXX --central /dev/cu.usbmodemYYYY
```

It joins the network `libs/wifi` saved in NVS (the partition table matches
`../development`'s). `wifi join <ssid> <password>` on the console saves and
joins another one.

| command | reply |
|---|---|
| `wifi` | `#OK wifi <state> <ip\|-> <rssi>` |
| `wifi ps none\|default` | `#OK wifi` |
| `wifi join <ssid> <password>` | `#OK wifi <result>` once the attempt ends |
| `heap` | `#OK heap <int_free> <int_min> <int_largest> <dma_free> <psram_free> <int_free_at_main>` |
| `sd <MB>` | `#OK sd <write KB/s> <read KB/s>`: writes, reads back and checks a file on the card |
| `ble ...` | as in `../development` |
