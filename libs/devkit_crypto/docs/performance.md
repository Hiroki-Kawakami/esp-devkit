# devkit_crypto performance

Measured with `firmware/rust_hello`'s `cryptobench` harness command, which times
each primitive on a task of its own. The ESP32-P4 board has a chip revision
below 3.0, whose ECDSA peripheral IDF does not use. The ESP32-S3 board has no
PSRAM.

Bulk throughput depends on where the data is. With PSRAM, a heap block above
`CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL` (16 KB by default) comes from PSRAM, and
the SHA and AES engines read it through DMA after a cache write-back.

| | ESP32-P4, internal RAM | ESP32-P4, PSRAM | ESP32-S3, internal RAM |
|---|---|---|---|
| AES-128-GCM seal, 16 KB | 40 MB/s | 4.0 MB/s | 3.7 MB/s |
| AES-256-GCM seal, 16 KB | 40 MB/s | 4.1 MB/s | 3.6 MB/s |
| ChaCha20-Poly1305 seal, 16 KB | 3.5 MB/s | 3.5 MB/s | 2.2 MB/s |
| SHA-256 | 82 MB/s | 46 MB/s | 74 MB/s |

The P4 does GCM in hardware (`SOC_AES_SUPPORT_GCM`); the S3 does only AES in
hardware and GHASH in software.

X25519, P-256/P-384 ECDSA verify and Ed25519 are this crate's; P-256 key
agreement and RSA are PSA's, with the P4's ECC and RSA hardware.

| | ESP32-P4 | ESP32-S3 |
|---|---|---|
| X25519 keygen + agree | 19 ms | 39 ms |
| P-256 keygen + agree | 5.6 ms | 140 ms |
| RSA-2048 verify | 16 ms | 19 ms |
| ECDSA P-256 verify | 17 ms | 54 ms |
| ECDSA P-384 verify | 54 ms | 173 ms |
| Ed25519 verify | 26 ms | 56 ms |

A full handshake with www.google.com (`https` harness command, Mozilla roots,
X25519; an ECDSA P-256 leaf under an intermediate that a P-384 root signed) took
150 ms on the P4 and 520 ms on the S3, and 104 ms and 215 ms once the
intermediate was in the verified-signature cache.
