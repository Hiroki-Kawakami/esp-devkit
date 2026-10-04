#!/bin/sh
# Host unit test for audio_framework. Run inside the nix dev shell, from
# anywhere:
#   nix develop -c esp-devkit/libs/audio_framework/test/run.sh
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/.." && pwd)
CC_DIR=$(CDPATH= cd -- "$ROOT/../../idf_compat" && pwd)
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

SRCS=$(find "$ROOT/src" -name '*.c')

gcc -std=c11 -Wall -Wextra -Werror -O2 \
    -I "$ROOT/inc" -I "$ROOT/src" -I "$CC_DIR/include" \
    "$DIR/audio_framework_test.c" $SRCS \
    "$CC_DIR/src/freertos_port.c" "$CC_DIR/src/freertos_queue.c" "$CC_DIR/src/freertos_task.c" \
    -lm -lpthread \
    -o "$OUT/audf_test"
"$OUT/audf_test"
