#!/usr/bin/env python3
# Builds and signs devkit_crypto CA bundles; see README.md.
import argparse
import calendar
import hashlib
import struct
import sys
from pathlib import Path

MAGIC = b"DKCA"
FORMAT = 1
SLOT_MAGIC = b"DKCS"
SECTOR = 4096


def parse_certdata(text):
    objects, obj, lines = [], None, iter(text.splitlines())
    for line in lines:
        parts = line.split(None, 2)
        if line.startswith("#") or len(parts) < 2:
            continue
        name, kind, *rest = parts
        if name == "CKA_CLASS":
            obj = {}
            objects.append(obj)
        if obj is None:
            continue
        if kind == "MULTILINE_OCTAL":
            data = bytearray()
            for octal in lines:
                if octal == "END":
                    break
                data += bytes(int(v, 8) for v in octal.split("\\")[1:])
            obj[name] = bytes(data)
        else:
            obj[name] = rest[0] if rest else ""
    return objects


def der_next(data, at):
    tag, first = data[at], data[at + 1]
    at += 2
    if first < 0x80:
        length = first
    else:
        n = first & 0x7F
        length = int.from_bytes(data[at:at + n], "big")
        at += n
    return tag, at, at + length


def subject_and_spki(cert):
    _, start, _ = der_next(cert, 0)
    _, at, _ = der_next(cert, start)
    if cert[at] == 0xA0:
        at = der_next(cert, at)[2]
    for _ in range(4):
        at = der_next(cert, at)[2]
    subject_end = der_next(cert, at)[2]
    spki_end = der_next(cert, subject_end)[2]
    return cert[at:subject_end], cert[subject_end:spki_end]


def unix_time(utc):
    text = utc.decode()
    year = int(text[:2])
    year += 2000 if year < 50 else 1900
    fields = [int(text[i:i + 2]) for i in range(2, 12, 2)]
    return calendar.timegm((year, *fields, 0, 0, 0))


def bundle(certdata, version):
    objects = parse_certdata(certdata)
    trust = {
        (o["CKA_ISSUER"], o["CKA_SERIAL_NUMBER"]): o.get("CKA_TRUST_SERVER_AUTH")
        for o in objects
        if o.get("CKA_CLASS") == "CKO_NSS_TRUST"
    }
    entries, count = bytearray(), 0
    for o in objects:
        if o.get("CKA_CLASS") != "CKO_CERTIFICATE":
            continue
        if trust.get((o["CKA_ISSUER"], o["CKA_SERIAL_NUMBER"])) != "CKT_NSS_TRUSTED_DELEGATOR":
            continue
        subject, spki = subject_and_spki(o["CKA_VALUE"])
        distrust = o.get("CKA_NSS_SERVER_DISTRUST_AFTER")
        after = unix_time(distrust) if isinstance(distrust, bytes) else 0
        entries += struct.pack("<HHq", len(subject), len(spki), after) + subject + spki
        count += 1
    return MAGIC + bytes([FORMAT, 0, 0, 0]) + struct.pack("<III", version, count, len(entries)) + bytes(entries)


def partition_image(data):
    slot = SLOT_MAGIC + struct.pack("<II", 1, len(data)) + hashlib.sha256(data).digest()
    return slot.ljust(SECTOR, b"\xff") + b"\xff" * SECTOR + data


def nss_version(text):
    major, minor = text.split(".")[:2]
    return int(major) * 1000 + int(minor)


def main():
    parser = argparse.ArgumentParser(description="devkit_crypto CA bundles")
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("build", help="write ca_bundle.bin and ca_partition.bin")
    p.add_argument("certdata")
    p.add_argument("nss_version", help="e.g. 3.123; the bundle version is major * 1000 + minor")
    p.add_argument("out")
    p = sub.add_parser("sign", help="append the Ed25519 signature of a bundle")
    p.add_argument("bundle")
    p.add_argument("seed", help="file holding the 32-byte private seed")
    p.add_argument("out")
    args = parser.parse_args()
    if args.command == "build":
        data = bundle(Path(args.certdata).read_text(), nss_version(args.nss_version))
        out = Path(args.out)
        out.mkdir(parents=True, exist_ok=True)
        (out / "ca_bundle.bin").write_bytes(data)
        (out / "ca_partition.bin").write_bytes(partition_image(data))
    else:
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

        data = Path(args.bundle).read_bytes()
        key = Ed25519PrivateKey.from_private_bytes(Path(args.seed).read_bytes())
        Path(args.out).write_bytes(data + key.sign(data))


if __name__ == "__main__":
    sys.exit(main())
