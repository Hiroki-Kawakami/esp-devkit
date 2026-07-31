#!/bin/sh
set -eu

TEST_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
TEST_TMP=$(mktemp -d "${TMPDIR:-/tmp}/esp-devkit-wifi-test.XXXXXX")
trap 'rm -rf -- "$TEST_TMP"' EXIT HUP INT TERM

CXX_BIN=${CXX:-c++}
CC_BIN=${CC:-cc}
CJSON_FLAGS=$(pkg-config --cflags --libs libcjson 2>/dev/null \
    || pkg-config --cflags --libs cjson)

for source in esp_err nvs freertos_port freertos_task freertos_queue freertos_timers
do
    "$CC_BIN" -std=c11 -Wall -Wextra -Werror \
        -I"$TEST_ROOT/idf_compat/include" \
        -c "$TEST_ROOT/idf_compat/src/$source.c" \
        -o "$TEST_TMP/$source.o"
done

"$CC_BIN" -std=c11 -Wall -Wextra -Werror \
    -I"$TEST_ROOT/idf_compat/include" \
    -c "$TEST_ROOT/sim_harness/sim_harness.c" \
    -o "$TEST_TMP/sim_harness.o"

"$CXX_BIN" -std=c++17 -Wall -Wextra -Werror \
    -I"$TEST_ROOT/libs/wifi/inc" \
    -I"$TEST_ROOT/libs/wifi/src" \
    -I"$TEST_ROOT/idf_compat/include" \
    -I"$TEST_ROOT/sim_harness" \
    "$TEST_ROOT/libs/wifi/test/test_wifi_manager.cpp" \
    "$TEST_ROOT/libs/wifi/src/wifi_manager.cpp" \
    "$TEST_ROOT/libs/wifi/src/backend_sim.cpp" \
    "$TEST_TMP/esp_err.o" \
    "$TEST_TMP/nvs.o" \
    "$TEST_TMP/freertos_port.o" \
    "$TEST_TMP/freertos_task.o" \
    "$TEST_TMP/freertos_queue.o" \
    "$TEST_TMP/freertos_timers.o" \
    "$TEST_TMP/sim_harness.o" \
    -pthread $CJSON_FLAGS \
    -o "$TEST_TMP/test_wifi_manager"

"$TEST_TMP/test_wifi_manager" "$TEST_TMP/nvs.json"
