#!/bin/sh
# Host tests for devkit_crypto. Run inside the nix dev shell, from anywhere.
# Generates the fixtures with the Python, cryptography and OpenSSL nix-shell
# brings in, starts the TLS 1.3 servers the TLS tests talk to (OpenSSL's, and a
# pyOpenSSL one that reports the exported keying material), and runs
# the crate's #[test]s in firmware/rust_hello's simulator build.
set -e

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$DIR/../../.." && pwd)
SIM=$ROOT/firmware/rust_hello/simulator
OUT=$(mktemp -d)
PIDS=""
trap 'kill $PIDS 2>/dev/null; rm -rf "$OUT"' EXIT

PYTHON=$(nix-shell -p 'python3.withPackages (ps: [ ps.cryptography ps.pyopenssl ])' --run 'command -v python3')
"$PYTHON" "$DIR/gen_fixtures.py" "$OUT"
OPENSSL=$(nix-shell -p openssl --run 'command -v openssl')
OPENSSL=$OPENSSL sh "$DIR/certs.sh" "$OUT"

mkfifo "$OUT/hold"
exec 3<>"$OUT/hold"
port=44330
serve() { # name key cert [s_server options]
    name=$1 key=$2 cert=$3
    shift 3
    (cd "$OUT" && exec "$OPENSSL" s_server -accept "$port" -tls1_3 -key "$key" -cert "$cert" \
        "$@" > "$OUT/$name.log" 2>&1 < "$OUT/hold") &
    PIDS="$PIDS $!"
    echo "$name $port" >> "$OUT/servers.txt"
    port=$((port + 1))
}
serve rsa leaf_rsa.key leaf_rsa.pem -cert_chain inter.pem -www
serve chacha leaf_ec.key leaf_ec.pem -www -ciphersuites TLS_CHACHA20_POLY1305_SHA256
serve ed25519 leaf_ed25519.key leaf_ed25519.pem -www -ciphersuites TLS_AES_256_GCM_SHA384
serve retry leaf_ec.key leaf_ec.pem -www -groups P-256
serve p384 leaf_p384.key leaf_p384.pem -www -groups P-384
serve pss leaf_pss.key leaf_pss.pem -cert_chain inter.pem -www
serve nc leaf_nc.key leaf_nc.pem -cert_chain chain_nc.pem -www
serve client_auth leaf_ec.key leaf_ec.pem -www -verify 1
serve files leaf_ec.key leaf_ec.pem -WWW
serve echo leaf_ec.key leaf_ec.pem -rev
(cd "$OUT" && exec "$PYTHON" "$DIR/exporter_server.py" "$port" leaf_ec.key leaf_ec.pem > "$OUT/exporter.log" 2>&1) &
PIDS="$PIDS $!"
echo "exporter $port" >> "$OUT/servers.txt"
sleep 1

cmake -G Ninja -S "$SIM" -B "$SIM/build"
DEVKIT_TEST_FIXTURES=$OUT cmake --build "$SIM/build" --target rust_test_devkit_crypto
