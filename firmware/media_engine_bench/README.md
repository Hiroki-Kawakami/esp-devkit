# media_engine_bench

Checks `libs/media_engine` against the IDF PPA driver and `jpeg_decode_enhanced`
on the same pictures: output checksums and time per operation. The JPEGs are
encoded on the board with `devkit_image`. The simulator has no C decoder and
checks the strips against the whole picture instead.

```sh
devkit run simulator
devkit run esp32p4 --port <board>
```
