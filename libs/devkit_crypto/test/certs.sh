#!/bin/sh
# Writes a test PKI into $1 with $OPENSSL: an ECDSA root, an RSA intermediate,
# leaves for localhost of every key type, a name-constrained intermediate and a
# PSS-signed leaf, each also as DER.
set -e
cd "$1"
O=$OPENSSL

cat > ca.cnf <<EOF
basicConstraints=critical,CA:TRUE
keyUsage=critical,keyCertSign,cRLSign
EOF
cat > nc.cnf <<EOF
basicConstraints=critical,CA:TRUE
keyUsage=critical,keyCertSign,cRLSign
nameConstraints=critical,permitted;DNS:example.com
EOF
cat > leaf.cnf <<EOF
basicConstraints=CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost,IP:127.0.0.1
EOF

$O ecparam -name prime256v1 -genkey -noout -out root.key
$O req -x509 -new -key root.key -subj "/CN=devkit test root" -days 3650 -out root.pem \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign

issue() { # name issuer extfile [x509 options]
    name=$1 issuer=$2 ext=$3
    shift 3
    $O req -new -key "$name.key" -subj "/CN=$name" -out "$name.csr"
    $O x509 -req -in "$name.csr" -CA "$issuer.pem" -CAkey "$issuer.key" -CAcreateserial -days 365 \
        -extfile "$ext" -out "$name.pem" "$@" 2>/dev/null
}

$O genrsa -out inter.key 2048 2>/dev/null
issue inter root ca.cnf
$O genrsa -out leaf_rsa.key 2048 2>/dev/null
issue leaf_rsa inter leaf.cnf
$O ecparam -name prime256v1 -genkey -noout -out leaf_ec.key
issue leaf_ec root leaf.cnf
$O ecparam -name secp384r1 -genkey -noout -out leaf_p384.key
issue leaf_p384 root leaf.cnf
$O genpkey -algorithm ed25519 -out leaf_ed25519.key
issue leaf_ed25519 root leaf.cnf
$O genrsa -out leaf_pss.key 2048 2>/dev/null
issue leaf_pss inter leaf.cnf -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:digest
$O ecparam -name prime256v1 -genkey -noout -out inter_nc.key
issue inter_nc root nc.cnf
$O ecparam -name prime256v1 -genkey -noout -out leaf_nc.key
issue leaf_nc inter_nc leaf.cnf

for name in root inter inter_nc leaf_rsa leaf_ec leaf_p384 leaf_ed25519 leaf_pss leaf_nc; do
    $O x509 -in "$name.pem" -outform DER -out "$name.der"
done
cat inter.pem root.pem > chain_rsa.pem
cat inter_nc.pem > chain_nc.pem
head -c 100000 /dev/urandom > big.bin
