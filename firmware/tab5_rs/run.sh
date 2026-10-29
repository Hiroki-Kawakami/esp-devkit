#!/bin/sh
# Build + flash + monitor the Rust bsp_rs firmware for the Tab5.
#   ./run.sh                # esp32p4 (M5Stack Tab5)
#   ./run.sh simulator      # host simulator in the simulator host window (docs/simulator.md)
# Any extra args after esp32p4 pass through to idf.py (e.g. `./run.sh esp32p4 build`),
# after simulator to sim_host (e.g. `./run.sh simulator --headless`).
# Assumes the toolchain is on PATH — run under `nix develop -c ./run.sh ...` or
# from inside a nix dev shell.
set -e

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
SIM_HOST="$HERE/../../tools/sim_host"
TARGET=${1:-esp32p4}
[ $# -gt 0 ] && shift

case "$TARGET" in
  esp32p4)
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/$TARGET" "$@"
    ;;
  simulator)
    [ -d "$HERE/simulator/build" ] || cmake --fresh -S "$HERE/simulator" -B "$HERE/simulator/build" -G Ninja
    cmake --build "$HERE/simulator/build"
    cargo build --release --manifest-path "$SIM_HOST/Cargo.toml"
    "$SIM_HOST/target/release/sim_host" "$@" "$HERE/simulator/build/simulator"
    ;;
  *)
    echo "Usage: $0 [esp32p4|simulator] [idf.py|sim_host args...]" >&2
    exit 1
    ;;
esac
