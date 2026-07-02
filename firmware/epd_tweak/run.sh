#!/bin/sh
# Build + flash + monitor the EPD tuning firmware.
#   ./run.sh                # esp32s3 (PaperS3 / ED047TC1, the actual tuning target)
#   ./run.sh esp32          # M5Paper / IT8951E vendor TCON (comparison)
# Any extra args pass through to idf.py (e.g. `./run.sh esp32s3 build`).
#
# Live tuning: after flashing, quit the monitor and open tool/index.html in
# Chrome/Edge -- it talks the SerialCmd.cpp protocol over Web Serial on the
# same port (log console), so waveform LUTs and test images can be pushed
# without a rebuild/reflash. LUT rewrite is paper_s3-only (epd_ll).
# Assumes `idf.py` is on PATH -- run under `nix develop -c ./run.sh ...` or from
# inside a nix dev shell (NameCardKnot's .envrc autoloads it via direnv).
set -e

TARGET=${1:-esp32s3}
[ $# -gt 0 ] && shift

case "$TARGET" in
  esp32s3|esp32)
    HERE=$(cd -- "$(dirname -- "$0")" && pwd)
    if [ $# -eq 0 ]; then
        set -- flash monitor
    fi
    idf.py -C "$HERE/$TARGET" "$@"
    ;;
  *)
    echo "Usage: $0 [esp32s3|esp32] [idf.py args...]" >&2
    exit 1
    ;;
esac
