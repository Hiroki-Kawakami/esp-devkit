#!/bin/sh
# Build + flash + monitor the Rust hello firmware.
#   ./run.sh                # esp32p4 (M5Stack Tab5)
#   ./run.sh esp32s3        # esp32s3 (M5Stack CoreS3)
#   ./run.sh simulator      # host simulator (headless; the app only logs)
#   ./run.sh test           # host unit tests of the Rust crates
# Any extra args after esp32p4/esp32s3 pass through to idf.py (e.g. `./run.sh esp32s3 build`).
# Assumes the toolchain is on PATH — run under `nix develop -c ./run.sh ...` or
# from inside a nix dev shell.
set -e

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
TARGET=${1:-esp32p4}
[ $# -gt 0 ] && shift

case "$TARGET" in
  esp32p4|esp32s3)
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/$TARGET" "$@"
    ;;
  simulator|test)
    [ -d "$HERE/simulator/build" ] || cmake --fresh -S "$HERE/simulator" -B "$HERE/simulator/build" -G Ninja
    if [ "$TARGET" = test ]; then
        cmake --build "$HERE/simulator/build" --target rust_test
    else
        cmake --build "$HERE/simulator/build"
        SIMULATOR_HEADLESS=1 "$HERE/simulator/build/simulator"
    fi
    ;;
  *)
    echo "Usage: $0 [esp32p4|esp32s3|simulator|test] [idf.py args...]" >&2
    exit 1
    ;;
esac
