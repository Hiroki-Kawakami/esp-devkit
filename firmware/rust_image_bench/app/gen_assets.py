#!/usr/bin/env python3
"""Writes a PNG and, when cjpeg and cwebp are on PATH, a progressive JPEG and two
WebPs of the same procedural picture into a directory. A file for a tool that is
missing is left empty and its case skipped.

The baseline JPEG the bench uses is encoded on the target at run time, so it
needs no asset.

Usage: gen_assets.py <dir> [width] [height] [grain]
"""
import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib


def picture(w, h, grain):
    """Photo-like rows: low-frequency colour field, mid-frequency structure,
    fine grain. The grain is what keeps the entropy stages honest."""
    rows = []
    s = 2463534242
    for y in range(h):
        row = bytearray()
        fy = y / h
        for x in range(w):
            fx = x / w
            p = (math.sin(fx * 9.0) + math.sin(fy * 7.0 + 1.3) +
                 0.5 * math.sin((fx + fy) * 15.0) +
                 0.4 * math.sin(math.hypot(fx - 0.4, fy - 0.6) * 22.0))
            s ^= (s << 13) & 0xFFFFFFFF
            s ^= s >> 17
            s ^= (s << 5) & 0xFFFFFFFF
            n = (((s >> 8) & 0x3F) - 32) * grain
            d = 5.0 * (math.sin(x * 2.1) * math.sin(y * 1.7) +
                        0.6 * math.sin((x + y) * 1.13) +
                        0.5 * math.sin((x - y) * 0.71)) * (0.35 + 0.65 * fy)
            for v in (128 + 36 * p + 60 * fx + n + d,
                      128 + 80 * math.sin(p * 1.7) + 50 * fy + n + d,
                      128 + 70 * math.cos(p * 1.1 + fx * 3.0) + n - d):
                row.append(0 if v < 0 else (255 if v > 255 else int(v)))
        rows.append(bytes(row))
    return rows


def png(w, h, rows):
    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data +
                struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    raw = bytearray()
    for r in rows:
        raw.append(1)  # Sub
        raw += bytes((r[i] - (r[i - 3] if i >= 3 else 0)) & 0xFF for i in range(len(r)))
    return (b"\x89PNG\r\n\x1a\n" +
            chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) +
            chunk(b"IDAT", zlib.compress(bytes(raw), 6)) +
            chunk(b"IEND", b""))


def progressive_jpeg(w, h, rows, quality=85):
    cjpeg = shutil.which("cjpeg")
    if not cjpeg:
        return None
    with tempfile.TemporaryDirectory() as td:
        ppm = os.path.join(td, "src.ppm")
        with open(ppm, "wb") as f:
            f.write(b"P6\n%d %d\n255\n" % (w, h))
            for r in rows:
                f.write(r)
        out = subprocess.run([cjpeg, "-quality", str(quality), "-progressive",
                              "-sample", "2x2", ppm],
                             capture_output=True, check=True)
        return out.stdout


def webp(w, h, rows, *options):
    cwebp = shutil.which("cwebp")
    if not cwebp:
        return None
    with tempfile.TemporaryDirectory() as td:
        ppm = os.path.join(td, "src.ppm")
        out = os.path.join(td, "out.webp")
        with open(ppm, "wb") as f:
            f.write(b"P6\n%d %d\n255\n" % (w, h))
            for r in rows:
                f.write(r)
        subprocess.run([cwebp, "-quiet", *options, ppm, "-o", out], check=True)
        with open(out, "rb") as f:
            return f.read()


def main():
    out_dir = sys.argv[1]
    w = int(sys.argv[2]) if len(sys.argv) > 2 else 960
    h = int(sys.argv[3]) if len(sys.argv) > 3 else 540
    grain = float(sys.argv[4]) if len(sys.argv) > 4 else 0.15

    rows = picture(w, h, grain)
    files = {
        "png.bin": png(w, h, rows),
        "progressive.bin": progressive_jpeg(w, h, rows),
        "webp_lossy.bin": webp(w, h, rows, "-q", "80"),
        "webp_lossless.bin": webp(w, h, rows, "-lossless"),
    }
    for name, data in files.items():
        with open(os.path.join(out_dir, name), "wb") as f:
            f.write(data or b"")


main()
