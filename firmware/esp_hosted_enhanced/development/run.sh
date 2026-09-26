#!/bin/sh
# Build + flash the esp_hosted_enhanced development firmware (M5Stack Tab5).
#   ./run.sh                # internal C6: idf.py flash monitor
#   ./run.sh xiao build     # XIAO ESP32C6 on the M-Bus
# Any args after the board pass through to idf.py.
# Assumes the toolchain is on PATH — run under `nix develop -c ./run.sh ...`.
set -e

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
BOARD=tab5
case "$1" in
  tab5|xiao) BOARD=$1; shift ;;
esac
if [ $# -eq 0 ]; then
    set -- flash monitor
fi

case "$BOARD" in
  tab5)
    idf.py -C "$HERE/esp32p4" "$@"
    ;;
  xiao)
    BUILD="$HERE/esp32p4/build_xiao"
    idf.py -C "$HERE/esp32p4" -B "$BUILD" -D SDKCONFIG="$BUILD/sdkconfig" \
        -D SDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.defaults.xiao" "$@"
    ;;
esac
