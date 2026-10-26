#!/bin/sh
# Build + flash + monitor the Rust hello firmware.
#   ./run.sh                # esp32p4 (M5Stack Tab5)
#   ./run.sh esp32s3        # esp32s3 (M5Stack CoreS3)
# Any extra args pass through to idf.py (e.g. `./run.sh esp32s3 build`).
# Assumes the toolchain is on PATH — run under `nix develop -c ./run.sh ...` or
# from inside a nix dev shell.
set -e

TARGET=${1:-esp32p4}
[ $# -gt 0 ] && shift

case "$TARGET" in
  esp32p4|esp32s3)
    HERE=$(cd -- "$(dirname -- "$0")" && pwd)
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/$TARGET" "$@"
    ;;
  *)
    echo "Usage: $0 [esp32p4|esp32s3] [idf.py args...]" >&2
    exit 1
    ;;
esac
