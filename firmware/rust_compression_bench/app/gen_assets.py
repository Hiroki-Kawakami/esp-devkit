#!/usr/bin/env python3
# Writes the bench's streams into a directory: raw.bin, the same bytes as a zlib
# stream, and a smaller alphabet of them as LZW in the GIF and TIFF variants.
import os
import sys
import zlib

SIZE = 1 << 20


def picture():
    out = bytearray()
    x = 12345
    while len(out) < SIZE:
        x = (x * 1103515245 + 12345) & 0x7FFFFFFF
        if out and x & 0x300000:
            back = 1 + (x >> 8) % min(len(out), 30000)
            length = 4 + (x >> 24) % 60
            for _ in range(length):
                out.append(out[-back])
        else:
            for _ in range(1 + (x & 15)):
                x = (x * 1103515245 + 12345) & 0x7FFFFFFF
                out.append((x >> 18) & 0x1F)
    return bytes(out[:SIZE])


def lzw(data, msb_first, early):
    bits = 8
    clear = 1 << bits
    table = {bytes([i]): i for i in range(clear)}
    next_code = clear + 2
    width = bits + 1
    out = bytearray()
    acc = 0
    nacc = 0

    def put(code):
        nonlocal acc, nacc
        if msb_first:
            acc = (acc << width) | code
            nacc += width
            while nacc >= 8:
                nacc -= 8
                out.append((acc >> nacc) & 0xFF)
            acc &= (1 << nacc) - 1
        else:
            acc |= code << nacc
            nacc += width
            while nacc >= 8:
                out.append(acc & 0xFF)
                acc >>= 8
                nacc -= 8

    put(clear)
    w = data[:1]
    for c in data[1:]:
        wc = w + bytes([c])
        if wc in table:
            w = wc
            continue
        put(table[w])
        table[wc] = next_code
        next_code += 1
        if next_code - (0 if early else 1) >= (1 << width) and width < 12:
            width += 1
        if next_code >= 4094:
            put(clear)
            table = {bytes([i]): i for i in range(clear)}
            next_code = clear + 2
            width = bits + 1
        w = bytes([c])
    put(table[w])
    put(clear + 1)
    if nacc:
        out.append((acc << (8 - nacc)) & 0xFF if msb_first else acc & 0xFF)
    return bytes(out)


def write(directory, name, data):
    with open(os.path.join(directory, name), "wb") as f:
        f.write(data)


directory = sys.argv[1]
raw = picture()
write(directory, "raw.bin", raw)
write(directory, "zlib.bin", zlib.compress(raw, 9))
write(directory, "gif.bin", lzw(raw, False, False))
write(directory, "tiff.bin", lzw(raw, True, True))
