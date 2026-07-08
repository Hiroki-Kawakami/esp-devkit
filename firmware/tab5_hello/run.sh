#!/bin/sh
# Build + run the Tab5 hello sample.
#   ./run.sh                # host SDL simulator (default)
#   ./run.sh esp32p4        # M5Stack Tab5: idf.py flash monitor
# Any extra args after esp32p4 pass through to idf.py (e.g. `./run.sh esp32p4 build`).
# Assumes the toolchain is on PATH — run under `nix develop -c ./run.sh ...` or
# from inside a nix dev shell.
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
  esp32p4)
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/$TARGET" "$@"
    ;;
  *)
    echo "Usage: $0 [simulator|esp32p4] [args...]" >&2
    exit 1
    ;;
esac
