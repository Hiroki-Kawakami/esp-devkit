#!/bin/sh
# Build & run the libs/sensors host unit test: the real SEN55/SCD40 drivers
# against their chip emulators over the idf_compat virtual I2C bus. Sleeps
# through the chips' real 1 s / 5 s measurement periods, so the run takes
# ~10 s. Run from anywhere.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/.." && pwd)                # esp-devkit/libs/sensors
CC_DIR=$(CDPATH= cd -- "$ROOT/../../idf_compat" && pwd)
OUT="$DIR/build"
mkdir -p "$OUT"

# idf_compat host shims, compiled as-is (no -Werror). Cached in build/.
objs=""
for s in i2c_master esp_timer freertos_port freertos_task freertos_queue; do
    o="$OUT/$s.o"
    [ -f "$o" ] || [ "$CC_DIR/src/$s.c" -ot "$o" ] || gcc -c "$CC_DIR/src/$s.c" -I"$CC_DIR/include" -o "$o"
    objs="$objs $o"
done

gcc -std=c11 -Wall -Wextra -Werror -O2 \
    -I "$ROOT/common" -I "$ROOT/sensirion" \
    -I "$ROOT/sensirion/sen55" -I "$ROOT/sensirion/scd40" -I "$CC_DIR/include" \
    "$DIR/sensors_test.c" \
    "$ROOT/common/sensor_i2c.c" "$ROOT/common/crc8.c" \
    "$ROOT/sensirion/sensirion_i2c.c" \
    "$ROOT/sensirion/sen55/sen55.c" "$ROOT/sensirion/sen55/sen55_sim.c" \
    "$ROOT/sensirion/scd40/scd40.c" "$ROOT/sensirion/scd40/scd40_sim.c" \
    $objs \
    -lm -lpthread \
    -o "$OUT/sensors_test"
"$OUT/sensors_test"
