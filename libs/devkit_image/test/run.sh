#!/bin/sh
# Host tests for devkit_image. Run inside the nix dev shell, from anywhere. The
# fixtures are generated first, with the encoders nix-shell brings in, and the
# crate's #[test]s run in firmware/rust_image_bench's simulator build.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/../../.." && pwd)
SIM=$ROOT/firmware/rust_image_bench/simulator
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

nix-shell -p libjpeg pkg-config libwebp ffmpeg-headless \
    'python3.withPackages (ps: [ ps.pillow ])' --run "
    set -e
    cd '$OUT'
    python3 '$DIR/gen_fixtures.py' '$OUT'
    python3 '$DIR/gen_pillow_fixtures.py' '$OUT'
    python3 '$DIR/gen_webp_fixtures.py' '$OUT'
    cc '$DIR/gen_jpeg.c' \$(pkg-config --cflags --libs libjpeg) -o gen_jpeg
    ./gen_jpeg
"

cmake -G Ninja -S "$SIM" -B "$SIM/build"
DEVKIT_TEST_FIXTURES=$OUT cmake --build "$SIM/build" --target rust_test_devkit_image
