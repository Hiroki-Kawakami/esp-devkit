#!/bin/sh
# Build + flash the esp_hosted_enhanced development firmware (M5Stack Tab5).
#   ./run.sh                        on-board C6, esp_hosted_enhanced host: idf.py flash monitor
#   ./run.sh xiao stock build       XIAO ESP32C6 on the M-Bus, stock esp-hosted host
# Board (tab5|xiao) and host (enhanced|stock) are optional, in that order; any
# remaining args pass through to idf.py.
# Assumes the toolchain is on PATH — run under `nix develop -c ./run.sh ...`.
set -e

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
BOARD=tab5
HOST=enhanced
case "$1" in
  tab5|xiao) BOARD=$1; shift ;;
esac
case "$1" in
  enhanced|stock) HOST=$1; shift ;;
esac
if [ $# -eq 0 ]; then
    set -- flash monitor
fi

DEFAULTS="sdkconfig.defaults"
[ "$HOST" = enhanced ] && DEFAULTS="$DEFAULTS;sdkconfig.defaults.enhanced"
[ "$BOARD" = xiao ] && DEFAULTS="$DEFAULTS;sdkconfig.defaults.xiao"
BUILD="$HERE/esp32p4/build_${BOARD}_${HOST}"
idf.py -C "$HERE/esp32p4" -B "$BUILD" -D SDKCONFIG="$BUILD/sdkconfig" \
    -D SDKCONFIG_DEFAULTS="$DEFAULTS" "$@"
