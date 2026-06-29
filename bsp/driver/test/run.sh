#!/bin/sh
# Host unit test for the pure transaction-index waveform core (epd_waveform.h):
# the SoC-independent logic only -- no ESP-IDF, no hardware. Run from anywhere.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

cc -std=c11 -Wall -Wextra "$DIR/epd_waveform_test.c" -o "$OUT/epdwf"
"$OUT/epdwf"
