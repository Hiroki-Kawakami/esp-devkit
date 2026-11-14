#!/bin/sh
# Regenerates src/tables.rs from nixpkgs' Unicode Character Database. Run inside
# the nix dev shell, from anywhere.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
UCD=$(nix-build '<nixpkgs>' -A unicode-character-database --no-out-link)/share/unicode
python3 "$DIR/gen_tables.py" "$UCD" "$DIR/../src/tables.rs"
