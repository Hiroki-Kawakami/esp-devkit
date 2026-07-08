#!/bin/sh
# Build + run the M5StickC-Plus hello sample.
#   ./run.sh                # host SDL simulator (default)
#   ./run.sh esp32          # M5StickC-Plus: idf.py flash monitor
# Any extra args after esp32 pass through to idf.py (e.g. `./run.sh esp32 build`).
#
# Assumes the toolchain is on PATH -- run the esp32 path from inside a nix dev
# shell (or `nix develop -c ./run.sh esp32`).
set -e

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
TARGET=${1:-simulator}
[ $# -gt 0 ] && shift

case "$TARGET" in
  simulator)
    [ -d "$HERE/simulator/build" ] || cmake --fresh -S "$HERE/simulator" -B "$HERE/simulator/build" -G Ninja
    cmake --build "$HERE/simulator/build"
    "$HERE/simulator/build/simulator"
    ;;
  esp32)
    export ESPBAUD=${ESPBAUD:-1500000}
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/$TARGET" "$@"
    ;;
  *)
    echo "Usage: $0 [simulator|esp32] [idf.py args...]" >&2
    exit 1
    ;;
esac
