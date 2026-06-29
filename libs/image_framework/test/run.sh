#!/bin/sh
# Host unit test for image_framework — the C decoders only: no ESP-IDF, no
# hardware. Builds with gcc -std=c11; -Werror keeps the public headers
# C-compatible. Run from anywhere.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/.." && pwd)
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

SRCS=$(find "$ROOT/src" -name '*.c')

gcc -std=c11 -Wall -Wextra -Werror -O2 \
    -I "$ROOT/inc" -I "$ROOT/src" \
    "$DIR/image_framework_test.c" $SRCS \
    -lm \
    -o "$OUT/imgf_test"
"$OUT/imgf_test"
