#!/bin/sh
# Build + flash + monitor the ATOM Lite hello firmware.
#   ./run.sh                # esp32   (M5Stack ATOM Lite)
#   ./run.sh esp32s3        # esp32s3 (M5Stack AtomS3 Lite)
# Any extra args pass through to idf.py (e.g. `./run.sh esp32 build`).
#
# Click the front button to cycle the RGB LED colour; long-press to turn it off.
# Assumes `idf.py` is on PATH -- run under `nix develop -c ./run.sh ...` or from
# inside a nix dev shell.
set -e

TARGET=${1:-esp32}
[ $# -gt 0 ] && shift

case "$TARGET" in
  esp32|esp32s3)
    HERE=$(cd -- "$(dirname -- "$0")" && pwd)
    [ "$TARGET" = esp32 ] && export ESPBAUD=${ESPBAUD:-115200}
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/$TARGET" "$@"
    ;;
  *)
    echo "Usage: $0 [esp32|esp32s3] [idf.py args...]" >&2
    exit 1
    ;;
esac
