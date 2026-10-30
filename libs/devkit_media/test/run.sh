#!/bin/sh
# Host tests for devkit_media. Run inside the nix dev shell, from anywhere.
# Generates the material with the tools nix-shell brings in, dumps what
# media_framework makes of each file, and runs the crate's #[test]s, which hold
# devkit_media to those dumps, in firmware/rust_media_bench's simulator build.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/../../.." && pwd)
SIM=$ROOT/firmware/rust_media_bench/simulator
C=$ROOT/libs/media_framework
COMPAT=$ROOT/idf_compat
OUT=${1:-$(mktemp -d)}
[ -n "$1" ] || trap 'rm -rf "$OUT"' EXIT

if [ ! -e "$OUT/.done" ]; then
    NIXPKGS_ALLOW_UNFREE=1 nix-shell -p ffmpeg lame flac opus-tools \
        --run "python3 '$DIR/gen_material.py' '$OUT'"
    touch "$OUT/.done"
fi

cmake -G Ninja -S "$SIM" -B "$SIM/build"

cc -std=gnu11 -O2 -g -w -DSIMULATOR \
    -I "$C/inc" -I "$C/src" -I "$COMPAT/include" -I "$(dirname "$(find "$SIM/build" -name sdkconfig.h | head -1)")" \
    "$DIR/dump_c.c" "$C"/src/*.c \
    "$COMPAT"/src/esp_log.c "$COMPAT"/src/esp_heap_caps.c "$COMPAT"/src/multi_heap.c "$COMPAT"/src/esp_timer.c \
    "$COMPAT"/src/freertos_port.c "$COMPAT"/src/freertos_queue.c "$COMPAT"/src/freertos_task.c \
    -lm -lpthread -o "$OUT/dump_c"

for f in "$OUT"/*.*; do
    case "$f" in *.cdump|*.rdump|*.dSYM) continue ;; esac
    timeout 60 "$OUT/dump_c" "$f" "$f.cdump" >/dev/null 2>&1 || echo "dump_c failed ($?): $f"
done

DEVKIT_TEST_FIXTURES=$OUT timeout 1200 cmake --build "$SIM/build" --target rust_test_devkit_media
