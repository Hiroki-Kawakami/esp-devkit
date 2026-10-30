#!/bin/sh
# Host tests for devkit_audio. Run inside the nix dev shell, from anywhere.
# Generates material with the audio_framework scripts into MATERIAL (a temporary
# directory unless set; the AAC script needs unfree fdk-aac), cuts it into
# packets, decodes those with audio_framework as the reference and runs the
# crate's #[test]s in firmware/rust_audio_bench's simulator build.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/../../.." && pwd)
SIM=$ROOT/firmware/rust_audio_bench/simulator
C=$ROOT/libs/audio_framework
CC_DIR=$ROOT/idf_compat
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

if [ -z "$MATERIAL" ]; then
    MATERIAL=$OUT/material
    for script in "$C"/test/make_*_material.sh; do
        "$script" "$MATERIAL"
    done
fi

printf '#define CONFIG_FREERTOS_HZ 1000\n#define CONFIG_FREERTOS_NUMBER_OF_CORES 2\n' > "$OUT/sdkconfig.h"
gcc -std=c11 -O2 -ffp-contract=off -fno-math-errno \
    -I "$C/inc" -I "$C/src" -I "$CC_DIR/include" -I "$OUT" \
    "$DIR/c_ref.c" "$C"/src/*.c \
    "$CC_DIR/src/freertos_port.c" "$CC_DIR/src/freertos_queue.c" "$CC_DIR/src/freertos_task.c" \
    -lm -lpthread -o "$OUT/c_ref"

mkdir -p "$OUT/fixtures"
python3 "$DIR/packetize.py" "$MATERIAL" "$OUT/fixtures"
for pkt in "$OUT"/fixtures/*.pkt; do
    "$OUT/c_ref" "$pkt" "${pkt%.pkt}.ref"
done

cmake -G Ninja -S "$SIM" -B "$SIM/build"
DEVKIT_TEST_FIXTURES=$OUT/fixtures cmake --build "$SIM/build" --target rust_test_devkit_audio
