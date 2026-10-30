# rust_media_bench

Times `libs/devkit_media` against `libs/media_framework` in one binary: each
demuxer opening a file, reading every packet and seeking, and the read-ahead buffer
alone. Each case runs 7 times and the best counts. A case whose packets differ
between the two logs an error.

```sh
devkit run esp32p4 --port <board>
```

The files live in memory behind a small VFS (`app/bench_files.c`), so the numbers
are the demuxers' and the buffer's and not a card's. `app/gen_assets.py` writes
them with noise for payloads, at the time of the build.

Tests for the crate run in this project's simulator build; see
`libs/devkit_media/test/run.sh`.
