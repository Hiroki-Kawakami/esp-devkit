#!/bin/sh
# Build + flash the esp_hosted_enhanced coprocessor firmware (ESP32-C6).
#   ./run.sh                # Tab5 on-board C6: idf.py build
#   ./run.sh xiao flash     # XIAO ESP32C6 over its own USB
# Any args after the board pass through to idf.py.
# Assumes the toolchain is on PATH — run under `nix develop -c ./run.sh ...`.
set -e

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
BOARD=tab5
case "$1" in
  tab5|xiao) BOARD=$1; shift ;;
esac
if [ $# -eq 0 ]; then
    set -- build
fi

BUILD="$HERE/build_$BOARD"
DEFAULTS="sdkconfig.defaults"
[ "$BOARD" = xiao ] && DEFAULTS="$DEFAULTS;sdkconfig.defaults.xiao"
idf.py -C "$HERE" -B "$BUILD" -D SDKCONFIG="$BUILD/sdkconfig" \
    -D SDKCONFIG_DEFAULTS="$DEFAULTS" "$@"
