# devkit_crypto

Cryptography for the firmware: hashes, MACs, KDFs, ciphers, AEADs, key
agreement, signatures, X.509, a sans-IO TLS 1.3 client and Mozilla's root CAs.
What mbedTLS's PSA Crypto API has is a thin wrapper over it, so the device gets
IDF's hardware accelerators and their locking; the rest is implemented here. The
simulator and host tests link the same PSA API from nixpkgs' `mbedtls_4`, the
version IDF ships.

```rust
use devkit_crypto::hash::{Hash, Sha256};
use devkit_crypto::kdf::Hkdf;
use devkit_crypto::mac::Hmac;

let digest = Sha256::digest(b"data");
let tag = Hmac::<Sha256>::mac(b"key", b"data");
let mut okm = [0; 32];
Hkdf::<Sha256>::new(b"salt", b"input key material").expand(&[b"label", b"context"], &mut okm)?;
```

| module | what | from |
|---|---|---|
| `hash` | the `Hash` trait; `Sha1`, `Sha256`, `Sha384`, `Sha512`; `HashAlg` for signatures | PSA |
| `hash` | `Blake2s`, keyed or not, 1 to 32 byte digests | own |
| `mac` | `Hmac<H>` over any `Hash`; `siphash` (SipHash-2-4) | own |
| `kdf` | `Hkdf<H>` over any `Hash` | own |
| `cipher` | `chacha20` from any block counter | own |
| `aead` | `Aead`: AES-128/256-GCM, ChaCha20-Poly1305 | PSA |
| `ecdh` | `EphemeralSecret`: X25519, P-256, P-384 | PSA |
| `ecdsa` | `SigningKey`, `VerifyingKey`: P-256, P-384, P-521; DER signature conversion | PSA |
| `rsa` | `PrivateKey` (generate, PKCS #1 DER, PSS and v1.5 signing, v1.5 over a digest already taken), `PublicKey` | PSA |
| `ed25519` | `Point` and `Scalar` on edwards25519, `SigningKey`, `VerifyingKey` (RFC 8032) | own |
| `der` | ASN.1 DER `Reader` and `Writer` | own |
| `x509` | `Certificate`, `PublicKey`, `TrustStore`, `verify_chain`, `CertificateBuilder` | own |
| `tls` | `Connection`, `ClientConfig`, `Verify`, `Identity` | own |
| `ca_bundle` | the default bundle, `install`, `install_unsigned` | own |
| `rand` | `fill` | PSA |
| `ct` | constant-time `eq`, `zeroize` | own |

- HMAC and HKDF are built on the `Hash` trait rather than PSA's, so one
  implementation serves the PSA hashes (hardware SHA on the device) and BLAKE2s.
- Hashes and `rand::fill` panic if PSA fails. With the algorithms enabled, that
  only happens on a hardware fault.
- A PSA operation lives in a `Box`: mbedTLS does not promise that one survives
  being moved. Cloning a hash state goes through `psa_hash_clone`.
- A key is a PSA volatile key, destroyed on drop. PSA's key store has a fixed
  number of slots, which bounds how many keys exist at once.
- `Aead` encrypts and decrypts in place, with the tag after the ciphertext. IDF
  builds PSA with `MBEDTLS_PSA_ASSUME_EXCLUSIVE_BUFFERS`, under which only an
  output that starts exactly at the input is safe; mbedTLS's own TLS relies on
  the same.
- The PSA setters for key attributes are `static inline`, which host tests do
  not link; the attributes are written field by field instead.
- The crate's Kconfig selects the `MBEDTLS_*` options it uses, so a firmware's
  sdkconfig cannot leave one out.

## Ed25519

`Point` and `Scalar` are the parts protocols build on: decoding with the RFC
8032 checks, addition, negation, multiplication by a `Scalar` or by
`mul_bits`, which takes a 256-bit integer as it is, without reducing it modulo
the group order, for protocols that multiply by a cofactor-cleared or clamped
value. Point operations take the same time for any input. The field arithmetic
uses 16-bit limbs, which keeps it simple and slow; what it costs on the devices
is in [`docs/performance.md`](docs/performance.md).

## X.509

`Certificate::parse` borrows the DER and fails on a critical extension it does
not know. `verify_chain` takes a server chain, leaf first, and finds a path to a
root in a `TrustStore`: validity periods (skipped when there is no clock),
basicConstraints and pathLenConstraint, keyUsage and extKeyUsage (serverAuth),
DNS and IP name constraints, the root's distrust-after date, and the server's
name against the subject alternative names (no Common Name fallback; a `*`
only as the whole leftmost label). A leaf whose key is itself in the store is
trusted as it is. Signatures: RSA PKCS #1 v1.5 and PSS, ECDSA, Ed25519.

`CertificateBuilder` writes version 3 certificates, self-signed or for another
key: subject `Name`, serial, validity, basicConstraints, DNS names.

## TLS

`Connection::client(config, server_name)` is a TLS 1.3 client without I/O.
Bytes from the peer go into `rx_space` and `commit_rx` (or `receive`), bytes for
the peer come out of `pending_tx` and `consume_tx`; `read` and `write` carry the
application data, and writes made during the handshake go out after it. It
starts wherever the transport stands, so STARTTLS is just a `Connection` on a
stream already in use; `devkit_net::TlsStream` runs one over TCP.

- Cipher suites: AES-128-GCM, ChaCha20-Poly1305 and AES-256-GCM, in the
  `ClientConfig` order. Key shares: X25519, P-256, P-384; the first goes in the
  ClientHello and a HelloRetryRequest switches to another.
- Server authentication: `Verify::WebPki(store)` (a chain to the store, naming
  the server), `PinnedSpki`, `Custom(verifier)`, or `None` for protocols that
  bind the channel some other way. The CertificateVerify signature is always
  checked against the leaf.
- Client authentication: an `Identity` (chain and RSA, ECDSA or Ed25519 key),
  sent when the server asks.
- `export_keying_material` is the RFC 8446 exporter. KeyUpdate is followed both
  ways; NewSessionTicket is ignored, so there is no resumption or early data.
- One record of receive buffer (16.6 KB) in `ClientConfig::buffer_caps`; sent
  records are encrypted into a buffer that grows with what is written.
- `clock` gives the time for validity periods; without one, they are not
  checked.

## CA bundle

The flake builds Mozilla's TLS server roots from the NSS release nixpkgs'
`cacert` pins and puts them in the dev shell's `DEVKIT_CA_BUNDLE`; what a bundle
holds, its version and how to build and sign one are in
[`tools/ca_bundle`](../../tools/ca_bundle/README.md).
`TrustStore::default_bundle()` returns it, where Kconfig placed it:

| `DEVKIT_CRYPTO_CA_BUNDLE` | |
|---|---|
| `NONE` (default) | `Unsupported` |
| `EMBEDDED` | in the app; changes only with the firmware |
| `PARTITION` | in the data partition `DEVKIT_CRYPTO_CA_PARTITION` (`ca_bundle`), written by `idf.py flash`, replaceable at run time |

The partition holds two slots after two sectors that describe them; 192 KiB
leaves room to grow:

```csv
ca_bundle, data, undefined, , 192K,
```

`install(data, key)` takes a bundle with its Ed25519 signature after it and
writes it when its version is newer; `install_unsigned(data)` writes without
either check. Both write the slot not in use and its sector last, so a reset
part way leaves the current bundle in place, and block for the flash erase and
write. `idf.py flash` rewrites both slot sectors, back to the bundle the
firmware was built with. A store keeps its slot mapped; installing into a slot
a store still maps fails with `Busy`. In the simulator the partition is the file
`<label>.partition` in the working directory, created from the build's image.

## Tests

```sh
nix develop -c libs/devkit_crypto/test/run.sh
```

`test/gen_fixtures.py` writes digests, MACs and HKDF output from Python's
`hashlib` and `hmac`, and AEAD, key agreement and signature vectors from the
cryptography package; RFC vectors are in the tests themselves. `test/certs.sh`
makes a test PKI with OpenSSL, and the TLS tests talk to OpenSSL's `s_server`
with each key type and cipher suite, a HelloRetryRequest, client
authentication and bulk data, and to `test/exporter_server.py` (pyOpenSSL) for
the exported keying material. The CA bundle tests need the dev shell's
`DEVKIT_CA_BUNDLE`.
