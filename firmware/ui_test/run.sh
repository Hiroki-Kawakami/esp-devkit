#!/bin/sh
set -e

HERE=$(cd -- "$(dirname -- "$0")" && pwd)
TARGET=${1:-tab5}
[ $# -gt 0 ] && shift

case "$TARGET" in
  paper|tab5)
    [ -d "$HERE/$TARGET/build" ] || cmake --fresh -S "$HERE/$TARGET" -B "$HERE/$TARGET/build" -G Ninja
    cmake --build "$HERE/$TARGET/build"
    "$HERE/$TARGET/build/simulator"
    ;;
  *)
    echo "Usage: $0 [paper|tab5]" >&2
    exit 1
    ;;
esac
