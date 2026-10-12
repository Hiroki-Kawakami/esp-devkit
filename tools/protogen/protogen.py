#!/usr/bin/env python3
"""Generate serialization code from a proto3 schema (stdlib only).

    protogen.py c|protobuf-c <file.proto> <outdir>
"""

import argparse
import sys
from pathlib import Path

import gen_c
import gen_protobuf_c
from proto import ProtoError, parse

GENERATORS = {
    "c": gen_c.generate,
    "protobuf-c": gen_protobuf_c.generate,
}


def write_if_changed(path, text):
    if path.exists() and path.read_text(encoding="utf-8") == text:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("lang", choices=sorted(GENERATORS))
    parser.add_argument("proto", type=Path)
    parser.add_argument("outdir", type=Path)
    args = parser.parse_args()

    try:
        proto = parse(args.proto)
    except ProtoError as e:
        print(f"protogen: {e}", file=sys.stderr)
        return 1

    for name, text in GENERATORS[args.lang](proto).items():
        write_if_changed(args.outdir / name, text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
