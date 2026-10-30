#!/bin/sh
# Host tests for devkit_compression. Run inside the nix dev shell, from
# anywhere. Generates the fixtures with the Pillow nix-shell brings in and runs
# the crate's #[test]s in firmware/rust_compression_bench's simulator build.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/../../.." && pwd)
SIM=$ROOT/firmware/rust_compression_bench/simulator
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

nix-shell -p 'python3.withPackages (ps: [ ps.pillow ])' \
    --run "python3 '$DIR/gen_fixtures.py' '$OUT'"

cmake -G Ninja -S "$SIM" -B "$SIM/build"
DEVKIT_TEST_FIXTURES=$OUT cmake --build "$SIM/build" --target rust_test_devkit_compression
