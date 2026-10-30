#!/usr/bin/env python3
# Writes small PNGs (built with the stdlib zlib encoder) into a directory, for the
# host tests of the PNG decoder and streaming inflate.
# run.sh runs it as: python3 gen_fixtures.py <dir>
import os
import struct
import sys
import zlib

OUT = sys.argv[1]


def png(width, height, colortype, bitdepth, rows, palette=None, trns=None):
    sig = b"\x89PNG\r\n\x1a\n"

    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data +
                struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    out = sig + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, bitdepth, colortype, 0, 0, 0))
    if palette is not None:
        out += chunk(b"PLTE", palette)
    if trns is not None:
        out += chunk(b"tRNS", trns)
    raw = b"".join(b"\x00" + bytes(r) for r in rows)  # filter type 0 per scanline
    out += chunk(b"IDAT", zlib.compress(raw, 9))
    out += chunk(b"IEND", b"")
    return out


def emit(name, data):
    with open(os.path.join(OUT, name + ".bin"), "wb") as f:
        f.write(data)


def emit_size(name, w, h):
    with open(os.path.join(OUT, name + ".size"), "w") as f:
        f.write(f"{w} {h}\n")


# 2x2 grayscale, 8-bit.
gray = png(2, 2, 0, 8, [[0, 255], [128, 64]])

# 2x2 truecolor, 8-bit: red, green / blue, white.
rgb = png(2, 2, 2, 8, [[255, 0, 0, 0, 255, 0], [0, 0, 255, 255, 255, 255]])

# 2x2 palette (4 colors), 8-bit indices.
pal = bytes([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255])
palette = png(2, 2, 3, 8, [[0, 1], [2, 3]], palette=pal)

# 2x2 truecolor+alpha: (200,100,50,128) over white, plus opaque/transparent samples.
rgba = png(2, 2, 6, 8, [[200, 100, 50, 128, 10, 20, 30, 255],
                        [0, 0, 0, 0, 90, 90, 90, 64]])

# 16x16 grayscale gradient — larger payload to exercise LZ77 back-references.
grad_rows = [[(x * 16 + y * 4) & 0xFF for x in range(16)] for y in range(16)]
grad = png(16, 16, 0, 8, grad_rows)

emit("kPngGray2x2", gray)
emit("kPngRgb2x2", rgb)
emit("kPngPalette2x2", palette)
emit("kPngRgba2x2", rgba)
emit("kPngGrad16", grad)
