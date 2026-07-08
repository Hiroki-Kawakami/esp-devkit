#!/bin/sh
# Build + run the M5 Air Quality Kit hello sample.
#   ./run.sh                # host SDL simulator (default)
#   ./run.sh esp32s3        # M5Stack Air Quality Kit: idf.py flash monitor
# Any extra args after esp32s3 pass through to idf.py (e.g. `./run.sh esp32s3 build`).
#
# Draws a test pattern to the GDEY0154D67 EPD on boot. Assumes the toolchain is
# on PATH — run under `nix develop -c ./run.sh ...` or from inside a nix dev shell.
set -e

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
TARGET=${1:-simulator}
[ $# -gt 0 ] && shift

case "$TARGET" in
  simulator)
    [ -d "$HERE/build" ] || cmake --fresh -S "$HERE/simulator" -B "$HERE/build" -G Ninja
    cmake --build "$HERE/build"
    "$HERE/build/simulator"
    ;;
  esp32s3)
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/$TARGET" "$@"
    ;;
  *)
    echo "Usage: $0 [simulator|esp32s3] [args...]" >&2
    exit 1
    ;;
esac
