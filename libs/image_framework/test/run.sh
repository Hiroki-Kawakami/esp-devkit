#!/bin/sh
# Host unit test for image_framework — the C decoders only: no ESP-IDF, no
# hardware. Builds with gcc -std=c11; -Werror keeps the public headers
# C-compatible. The fixtures are generated first, with the encoders nix-shell
# brings in. Run from anywhere.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/.." && pwd)
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

nix-shell -p libjpeg pkg-config libwebp ffmpeg-headless \
    'python3.withPackages (ps: [ ps.pillow ])' --run "
    set -e
    cd '$OUT'
    python3 '$DIR/gen_fixtures.py' > png_fixtures.h
    python3 '$DIR/gen_pillow_fixtures.py' > pillow_fixtures.h
    python3 '$DIR/gen_webp_fixtures.py' > webp_fixtures.h
    cc '$DIR/gen_jpeg.c' \$(pkg-config --cflags --libs libjpeg) -o gen_jpeg
    ./gen_jpeg > jpeg_fixtures.h
"

CMPF=$(CDPATH= cd -- "$ROOT/../compression_framework" && pwd)
SRCS=$(find "$ROOT/src" "$CMPF/src" -name '*.c')

gcc -std=c11 -Wall -Wextra -Werror -O2 \
    -I "$ROOT/inc" -I "$ROOT/src" -I "$CMPF/inc" -I "$OUT" \
    "$DIR/image_framework_test.c" $SRCS \
    -lm -lpthread \
    -o "$OUT/imgf_test"
"$OUT/imgf_test"
