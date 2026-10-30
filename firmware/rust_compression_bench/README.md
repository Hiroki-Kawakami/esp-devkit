# rust_compression_bench

Decodes a 1 MiB payload as a zlib stream and as LZW in the GIF and TIFF variants,
with `libs/devkit_compression` and `libs/compression_framework` in one binary,
and prints the best of 7 rounds of each and whether the output matched.

```sh
devkit run simulator
devkit run esp32p4 --port <board>
```

`app/gen_assets.py` writes the streams at build time. The input is in PSRAM, the
window and the tables in internal RAM, and the output arrives 4 KB at a time.
The firmware builds at `-Os` as an app does; both libraries raise themselves to
`-O2`.
