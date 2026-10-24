#!/bin/sh
# Host unit test for compression_framework. Builds with gcc -std=c11; -Werror
# keeps the public headers C-compatible. The fixtures are generated first, with
# the Pillow nix-shell brings in. Run from anywhere.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/.." && pwd)
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

nix-shell -p 'python3.withPackages (ps: [ ps.pillow ])' \
    --run "python3 '$DIR/gen_fixtures.py' > '$OUT/cmpf_fixtures.h'"

gcc -std=c11 -Wall -Wextra -Werror -O2 \
    -I "$ROOT/inc" -I "$OUT" \
    "$DIR/compression_framework_test.c" "$ROOT"/src/*.c \
    -o "$OUT/cmpf_test"
"$OUT/cmpf_test"
