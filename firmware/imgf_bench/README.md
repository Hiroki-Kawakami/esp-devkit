# imgf_bench

End-to-end timings for `libs/image_framework`: JPEG (baseline and progressive)
and PNG decode, resize, and the decode+resize pipeline. The same source builds
for the host and for the ESP32-P4, so a change can be measured on both.

```sh
nix develop -c ./run.sh              # host (gcc -O2)
nix develop -c ./run.sh esp32p4 -p /dev/cu.usbmodemXXXX flash
nix develop -c ./run.sh esp32p4 monitor
```

The host column uses `-O2` and the device column `-Os`, so the two are
baselines for themselves, not for each other.

Each case also prints a checksum of its output. The host build has no PIE
kernels, so a device run whose checksums match the host's is the proof that
every kernel writes what its C counterpart does.

## Fixture

The 1920x1080 source picture is drawn on the target with integer arithmetic, so
it is identical everywhere, and the baseline JPEGs are encoded at run time by
`imgf_jpege`. Its near-Nyquist texture is deliberate: plain grain is quantised
away at normal quality and would leave the entropy stages with unrealistically
little work. At quality 85 it lands around 1.5 bpp, in the range of a detailed
photograph.

`gen_assets.py` draws the same picture on the build machine for the two formats
image_framework cannot produce: a PNG (through zlib) and a progressive JPEG
(through `cjpeg`). Without `cjpeg` on PATH the progressive case is skipped and
the rest of the bench still runs.
