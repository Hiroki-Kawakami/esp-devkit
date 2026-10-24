#!/bin/sh
# Host unit test for compression_framework. Builds with gcc -std=c11; -Werror
# keeps the public headers C-compatible. Run from anywhere.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/.." && pwd)
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

gcc -std=c11 -Wall -Wextra -Werror -O2 \
    -I "$ROOT/inc" \
    "$DIR/compression_framework_test.c" "$ROOT"/src/*.c \
    -o "$OUT/cmpf_test"
"$OUT/cmpf_test"
