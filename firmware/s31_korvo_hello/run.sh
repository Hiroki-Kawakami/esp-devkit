#!/bin/sh
# Build + run the ESP32-S31-Korvo hello sample.
#   ./run.sh                # host SDL simulator (default)
#   ./run.sh esp32s31       # ESP32-S31-Korvo: idf.py flash monitor
# Any extra args after esp32s31 pass through to idf.py (e.g. `./run.sh esp32s31 build`).
#
# The esp32s31 target requires IDF 6.1 (still in beta). This script re-execs
# itself under `nix develop .#beta` when needed; the simulator path stays on
# the default shell. Assumes the toolchain is on PATH otherwise.
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
  esp32s31)
    if [ "$ESP_IDF_VERSION" != "6.1" ]; then
        FLAKE_ROOT=$(cd -- "$HERE/../.." && pwd)
        exec nix develop "path:$FLAKE_ROOT#beta" -c "$0" esp32s31 "$@"
    fi
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/$TARGET" "$@"
    ;;
  *)
    echo "Usage: $0 [simulator|esp32s31] [args...]" >&2
    exit 1
    ;;
esac
