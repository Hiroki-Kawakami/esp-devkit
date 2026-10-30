# rust_image_bench

Times `libs/devkit_image` against `libs/image_framework` in one binary: JPEG encode,
baseline, progressive and PNG decode, decode+resize, resize, and WebP decode. Each
line prints the best of 3 rounds of each, the ratio, and whether their output
checksums match.

```sh
devkit run simulator
devkit run esp32p4 --port <board>
```

The simulator has no PIE kernels, so a device run whose checksums equal the
simulator's is the proof that every kernel writes what its Rust counterpart does.

The 1920x1080 source picture is drawn on the target with integer arithmetic, so it
is identical everywhere, and the baseline JPEGs are encoded at run time. Its
near-Nyquist texture is deliberate: plain grain is quantised away at normal quality
and would leave the entropy stages with too little work. `app/gen_assets.py` draws a
similar picture at build time for the rest: a PNG, and with `cjpeg` and `cwebp` on
`PATH` a progressive JPEG and two WebPs. A case whose tool was missing is skipped.

The firmware builds at `-Os` as an app does; both libraries raise themselves to `-O2`.
