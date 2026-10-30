# rust_audio_bench

Decodes the same clips with `libs/devkit_audio` and `libs/audio_framework` in one
binary and prints each one's cost as a percentage of one core, best of 3 rounds,
and whether their PCM hashes agree. It also runs the PIE kernel selftests and an
EQ and gain chain.

The clips come from outside the repository. Build them from generated material,
then point `AUDIO_BENCH_CLIPS` at them for the build:

```sh
libs/audio_framework/test/make_mp3_material.sh /tmp/material    # and aac, opus, flac, alac
python3 libs/devkit_audio/test/packetize.py /tmp/material /tmp/packets
python3 firmware/rust_audio_bench/app/make_clips.py /tmp/packets /tmp/clips
AUDIO_BENCH_CLIPS=/tmp/clips devkit run esp32p4 --port <board>
```

The clips and the decoders are in PSRAM, the PCM in `Caps::SIMD` memory, and the
task runs on core 1 with a `Caps::SIMD` stack. The firmware builds at `-Os` as an
app does; both libraries raise themselves to `-O2`.

The simulator builds `audio_framework` without `-ffp-contract=off`, so on a host
that fuses multiply-adds the AAC and CELT hashes differ there; `libs/devkit_audio`
does not fuse them.
