#!/bin/sh
# Build + run the image_framework decode/resize benchmark.
#   ./run.sh                # host build (gcc), prints the table
#   ./run.sh esp32p4        # ESP32-P4 board: idf.py flash monitor
# Any extra args after esp32p4 pass through to idf.py.
# Assumes the toolchain is on PATH — run under `nix develop -c ./run.sh ...`.
#
# The host build uses -O2 while the device build uses IDF's default -Os, so the
# two columns are not directly comparable; each is a baseline for itself.
set -e

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$HERE/../.." && pwd)
TARGET=${1:-host}
[ $# -gt 0 ] && shift

case "$TARGET" in
  host)
    OUT="$HERE/build/host"
    mkdir -p "$OUT"
    python3 "$HERE/app/gen_assets.py" "$OUT/imgf_bench_assets.h" 960 540
    IMGF="$ROOT/libs/image_framework"
    gcc -std=c11 -Wall -Wextra -O2 \
        -I "$IMGF/inc" -I "$IMGF/src" -I "$HERE/app" -I "$OUT" \
        "$HERE/host/main.c" "$HERE/app/imgf_bench.c" "$IMGF"/src/*.c \
        -lm -lpthread \
        -o "$OUT/imgf_bench"
    "$OUT/imgf_bench"
    ;;
  esp32p4)
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/esp32p4" "$@"
    ;;
  *)
    echo "Usage: $0 [host|esp32p4] [args...]" >&2
    exit 1
    ;;
esac
