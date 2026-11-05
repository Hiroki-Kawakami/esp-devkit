# ca_bundle

Builds the CA bundles `devkit_crypto` trusts and signs them for
`ca_bundle::install`. Only Python's standard library is needed, except for
`sign`, which needs the cryptography package.

The flake runs `build` on the `certdata.txt` of the NSS release nixpkgs'
`cacert` pins and puts the output directory in the dev shell's
`DEVKIT_CA_BUNDLE`, where `devkit_crypto`'s build picks it up. Run it by hand to
make a bundle from another release, for a device to install at run time.

## Commands

```sh
python3 tools/ca_bundle/ca_bundle.py build <certdata.txt> <nss version> <out dir>
python3 tools/ca_bundle/ca_bundle.py sign <bundle> <seed> <out>
```

`build` writes two files into `<out dir>`:

- `ca_bundle.bin`, the bundle: the roots NSS trusts for TLS servers
  (`CKT_NSS_TRUSTED_DELEGATOR` for server authentication), with their
  distrust-after dates. Its version is the NSS version given, major * 1000 +
  minor (`3.123` is 3123).
- `ca_partition.bin`, the image `idf.py flash` writes to the CA bundle
  partition: the bundle in the first slot, the second slot empty.

`sign` reads a bundle and writes it with its Ed25519 signature appended, as
`install` takes it. `<seed>` is a file holding the 32-byte private key; the
device checks with the matching public key the app passes to `install`. Keep the
seed out of the repository.

```sh
nix-shell -p 'python3.withPackages (ps: [ ps.cryptography ])' --run \
    "python3 tools/ca_bundle/ca_bundle.py sign \$DEVKIT_CA_BUNDLE/ca_bundle.bin seed.bin signed.bin"
```

`install` accepts only a bundle newer than the one in place, so signing the one
the firmware was built with is only of use to a device holding an older one.

## Formats

All integers are little-endian.

Bundle:

| offset | size | |
|---|---|---|
| 0 | 4 | `DKCA` |
| 4 | 1 | format, 1 |
| 5 | 3 | zero |
| 8 | 4 | version |
| 12 | 4 | number of roots |
| 16 | 4 | length of the entries |
| 20 | | entries |

Each entry is the subject's length (u16), the SubjectPublicKeyInfo's length
(u16), the distrust-after time in Unix seconds (i64, 0 for none), then the
subject's and the SubjectPublicKeyInfo's DER. A signed bundle has 64 bytes of
signature, over everything before it, after the entries.

Partition: sectors 0 and 1 describe slots A and B, and the slots follow from
offset 8192, each half of what is left rounded down to whole sectors. A slot's
sector holds `DKCS`, a sequence number (u32), the bundle's length (u32) and its
SHA-256; an erased sector means an empty slot. The intact slot with the higher
sequence number is the current one.
