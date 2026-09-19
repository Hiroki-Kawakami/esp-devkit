#!/bin/sh
# Host test for jpeg_ppa_pipeline's out_clip. Run inside the nix dev shell,
# from anywhere:
#   nix develop -c esp-devkit/libs/jpeg_decode_enhanced/test/run.sh
#
# Layer 1 is replaced by a stub that feeds a synthetic RGB888 image in 16-row
# strips; PPA is the idf_compat CPU shim. Unclipped output must hash the same
# as the committed (HEAD) pipeline; clipped output must stay inside the clip.
# Build artifacts go to test/build/ (gitignored).
set -e

here=$(CDPATH= cd "$(dirname "$0")" && pwd)
comp=$(CDPATH= cd "$here/.." && pwd)
cc="$comp/../../idf_compat"
out="$here/build"
mkdir -p "$out/head/include" "$out/head/src"

srcs="$comp/src/ppa_srm_fast_host.c $cc/src/ppa.c $cc/src/esp_heap_caps.c $cc/src/esp_err.c \
      $cc/src/freertos_port.c $cc/src/freertos_queue.c $cc/src/freertos_task.c"
flags="-std=gnu11 -O1 -I$cc/include -I$comp/src"

gcc $flags -DHAVE_OUT_CLIP -I"$comp/include" \
    "$here/test_clip.c" "$comp/src/jpeg_ppa_pipeline.c" $srcs -lm -lpthread -o "$out/test_clip"

rel=$(cd "$comp" && git rev-parse --show-prefix)
(cd "$comp" && git show "HEAD:./include/jpeg_ppa_pipeline.h") > "$out/head/include/jpeg_ppa_pipeline.h"
(cd "$comp" && git show "HEAD:./include/jpeg_decode_enhanced.h") > "$out/head/include/jpeg_decode_enhanced.h"
(cd "$comp" && git show "HEAD:./src/jpeg_ppa_pipeline.c") > "$out/head/src/jpeg_ppa_pipeline.c"
gcc $flags -I"$out/head/include" \
    "$here/test_clip.c" "$out/head/src/jpeg_ppa_pipeline.c" $srcs -lm -lpthread -o "$out/test_clip_head"

echo "[run] unclipped output vs HEAD ($rel)"
"$out/test_clip_head" --dump > "$out/head.txt"
"$out/test_clip" --dump > "$out/new.txt"
if ! cmp -s "$out/head.txt" "$out/new.txt"; then
    diff "$out/head.txt" "$out/new.txt" | head -20
    echo "FAIL: unclipped output differs from HEAD"
    exit 1
fi
echo "$(wc -l < "$out/new.txt") unclipped cases identical"

echo "[run] clip"
exec "$out/test_clip"
