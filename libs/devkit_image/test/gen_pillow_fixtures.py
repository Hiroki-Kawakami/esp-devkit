#!/usr/bin/env python3
# Writes small BMP, GIF and TIFF files into a directory, each with the RGB (or
# gray) pixels Pillow decodes it to as the expected output. Files Pillow cannot
# write (RLE, bitfields, OS/2 headers, ...) are assembled here and only decoded
# by Pillow.
# run.sh runs it as: python3 gen_pillow_fixtures.py <dir>
import io
import os
import struct
import sys
import zlib

from PIL import Image

OUT = sys.argv[1]


def emit(name, data):
    with open(os.path.join(OUT, name + ".bin"), "wb") as f:
        f.write(data)


def emit_size(name, w, h):
    with open(os.path.join(OUT, name + ".size"), "w") as f:
        f.write(f"{w} {h}\n")


def emit_case(name, data, mode="RGB", expected=None):
    if expected is None:
        image = Image.open(io.BytesIO(data))
        image.load()
        if "transparency" in image.info or image.mode in ("RGBA", "LA", "RGBa"):
            white = Image.new("RGBA", image.size, (255, 255, 255, 255))
            expected = Image.alpha_composite(white, image.convert("RGBA")).convert(mode)
        else:
            expected = image.convert(mode)
    emit(f"k{name}", data)
    emit(f"k{name}Pixels", expected.tobytes())
    emit_size(f"k{name}", expected.width, expected.height)


def rgb_pattern(w, h):
    return [[((x * 40 + y * 7) & 255, (x * 3 + y * 50) & 255, (x * y * 11) & 255)
             for x in range(w)] for y in range(h)]


def palette_colors(n):
    return [((i * 37) & 255, (i * 91) & 255, (i * 13 + 5) & 255) for i in range(n)]


# ---- BMP --------------------------------------------------------------------

def pad4(row):
    return row + bytes(-len(row) % 4)


def pack_indices(indices, bpp):
    out = bytearray()
    per = 8 // bpp
    for i in range(0, len(indices), per):
        b = 0
        for j, v in enumerate(indices[i:i + per]):
            b |= v << (8 - bpp * (j + 1))
        out.append(b)
    return bytes(out)


def bmp(w, h, bpp, pixel_data, compression=0, palette=(), masks=None, header=40,
        top_down=False):
    if header == 12:
        info = struct.pack("<IHHHH", 12, w, h, 1, bpp)
        pal = b"".join(bytes((c[2], c[1], c[0])) for c in palette)
        extra = b""
    else:
        info = struct.pack("<IiiHHIIiiII", header, w, -h if top_down else h, 1, bpp,
                           compression, len(pixel_data), 2835, 2835, len(palette), 0)
        extra = b""
        if masks is not None:
            packed = struct.pack("<III", *masks)
            if header == 40:
                extra = packed
            else:
                info += packed + bytes(4)
        info += bytes(header - len(info))
        pal = b"".join(bytes((c[2], c[1], c[0], 0)) for c in palette)
    offset = 14 + len(info) + len(extra) + len(pal)
    head = b"BM" + struct.pack("<IHHI", offset + len(pixel_data), 0, 0, offset)
    return head + info + extra + pal + pixel_data


def rows_in_file(rows, top_down):
    return b"".join(pad4(r) for r in (rows if top_down else reversed(rows)))


def bmp_indexed(w, h, bpp, top_down=False, header=40):
    n = 1 << bpp
    rows = [pack_indices([(x + 2 * y) % n for x in range(w)], bpp) for y in range(h)]
    return bmp(w, h, bpp, rows_in_file(rows, top_down), palette=palette_colors(n),
               header=header, top_down=top_down)


def bmp_24(w, h, top_down=False, header=40):
    rows = [b"".join(bytes((b, g, r)) for r, g, b in line) for line in rgb_pattern(w, h)]
    return bmp(w, h, 24, rows_in_file(rows, top_down), header=header, top_down=top_down)


def bmp_16(w, h, masks):
    shifts = []
    for m in masks:
        s = (m & -m).bit_length() - 1
        shifts.append((s, (m >> s).bit_length()))
    rows = []
    for line in rgb_pattern(w, h):
        out = bytearray()
        for px in line:
            v = 0
            for (s, bits), c in zip(shifts, px):
                v |= (c >> (8 - bits)) << s
            out += struct.pack("<H", v)
        rows.append(bytes(out))
    if masks == (0x7C00, 0x03E0, 0x001F):
        return bmp(w, h, 16, rows_in_file(rows, False))
    return bmp(w, h, 16, rows_in_file(rows, False), compression=3, masks=masks)


def bmp_32_bitfields(w, h, header):
    rows = [b"".join(struct.pack("<I", (r << 24) | (g << 16) | (b << 8) | 0x80)
                     for r, g, b in line) for line in rgb_pattern(w, h)]
    return bmp(w, h, 32, rows_in_file(rows, True), compression=3,
               masks=(0xFF000000, 0x00FF0000, 0x0000FF00), header=header, top_down=True)


def bmp_32_rgb(w, h):
    rows = [b"".join(bytes((b, g, r, 0)) for r, g, b in line) for line in rgb_pattern(w, h)]
    return bmp(w, h, 32, rows_in_file(rows, False))


def bmp_rle8():
    w, h = 9, 4
    data = bytearray()
    data += bytes((3, 1, 2, 2))                   # bottom row: 1 1 1 2 2
    data += bytes((0, 3, 4, 5, 6, 0))             # absolute 4 5 6 + pad
    data += bytes((0, 0))                         # end of line
    data += bytes((0, 2, 2, 1))                   # delta: x += 2, y += 1
    data += bytes((4, 7))                         # row 2 from x = 2
    data += bytes((0, 0))
    data += bytes((9, 3))                         # top row
    data += bytes((0, 1))                         # end of bitmap
    return bmp(w, h, 8, bytes(data), compression=1, palette=palette_colors(8))


def bmp_rle4():
    w, h = 7, 3
    data = bytearray()
    data += bytes((5, 0x12))                      # 1 2 1 2 1
    data += bytes((0, 0))
    # Pillow misreads an absolute run of an odd number of nibbles.
    data += bytes((0, 6, 0x34, 0x56, 0x77, 0))    # absolute 3 4 5 6 7 7 + pad to a word
    data += bytes((1, 0x66))
    data += bytes((0, 0))
    data += bytes((7, 0x7A))
    data += bytes((0, 1))
    return bmp(w, h, 4, bytes(data), compression=2, palette=palette_colors(16))


# ---- GIF --------------------------------------------------------------------

def gif_from_pillow(w, h, **save):
    image = Image.new("P", (w, h))
    image.putpalette(bytes(c for color in palette_colors(256) for c in color))
    image.putdata([(x * 7 + y * 31 + x * y) & 255 for y in range(h) for x in range(w)])
    buf = io.BytesIO()
    image.save(buf, "GIF", optimize=False, **save)
    return buf.getvalue()


def gif_animated(w, h):
    frames = []
    for i in range(2):
        image = Image.new("RGB", (w, h), (40 * i, 200, 90))
        image.putdata([((x * 30 + i * 100) & 255, (y * 40) & 255, 60) for y in range(h)
                       for x in range(w)])
        frames.append(image)
    buf = io.BytesIO()
    frames[0].save(buf, "GIF", save_all=True, append_images=frames[1:], duration=100, loop=0,
                   comment=b"first frame only")
    return buf.getvalue()


def lzw_narrow(data, bits):
    clear = 1 << bits
    table = {bytes([i]): i for i in range(clear)}
    next_code = clear + 2
    width = bits + 1
    out = bytearray()
    acc = 0
    nacc = 0

    def put(code):
        nonlocal acc, nacc
        acc |= code << nacc
        nacc += width
        while nacc >= 8:
            out.append(acc & 0xFF)
            acc >>= 8
            nacc -= 8

    put(clear)
    w = data[:1]
    for c in data[1:]:
        if w + bytes([c]) in table:
            w += bytes([c])
            continue
        put(table[w])
        table[w + bytes([c])] = next_code
        next_code += 1
        if next_code > (1 << width):
            width += 1
        w = bytes([c])
    put(table[w])
    put(clear + 1)
    if nacc:
        out.append(acc & 0xFF)
    return bytes(out)


def gif_offset_frame():
    """A 4-colour frame smaller than the screen, placed at (2, 1), with colour 3
    transparent. Pillow fills the rest of the screen with the background colour;
    the decoder shows white there, so the expected pixels are built here."""
    sw, sh, left, top, fw, fh = 9, 6, 2, 1, 5, 3
    colors = [(200, 0, 0), (0, 150, 0), (0, 0, 250), (9, 9, 9)]
    indices = bytes((x + y) % 4 for y in range(fh) for x in range(fw))
    gif = b"GIF89a" + struct.pack("<HHBBB", sw, sh, 0x81, 0, 0)
    gif += bytes(c for color in colors for c in color)
    gif += b"\x21\xf9\x04\x01\x00\x00\x03\x00"
    gif += b"\x2c" + struct.pack("<HHHHB", left, top, fw, fh, 0) + b"\x02"
    stream = lzw_narrow(indices, 2)
    for i in range(0, len(stream), 255):
        gif += bytes([len(stream[i:i + 255])]) + stream[i:i + 255]
    gif += b"\x00\x3b"

    image = Image.open(io.BytesIO(gif)).convert("RGBA")
    for y in range(fh):
        for x in range(fw):
            idx = indices[y * fw + x]
            got = image.getpixel((left + x, top + y))
            assert got[3] == 0 if idx == 3 else got == colors[idx] + (255,)
    expected = Image.new("RGB", (sw, sh), (255, 255, 255))
    for y in range(fh):
        for x in range(fw):
            idx = indices[y * fw + x]
            if idx != 3:
                expected.putpixel((left + x, top + y), colors[idx])
    return gif, expected


# ---- TIFF -------------------------------------------------------------------

def tiff_from_pillow(image, **save):
    buf = io.BytesIO()
    image.save(buf, "TIFF", **save)
    return buf.getvalue()


def pattern_image(mode, w, h):
    image = Image.new("RGBA", (w, h))
    image.putdata([((x * 40 + y * 7) & 255, (x * 3 + y * 50) & 255, (x * y * 11) & 255,
                    (x * 60 + y * 20) & 255) for y in range(h) for x in range(w)])
    if mode == "P":
        return image.convert("RGB").quantize(colors=32)
    return image.convert(mode)


def differenced(row, spp, bps, big):
    if bps == 8:
        out = bytearray(row)
        for i in range(len(row) - 1, spp - 1, -1):
            out[i] = (row[i] - row[i - spp]) & 255
        return bytes(out)
    fmt = ">" if big else "<"
    values = list(struct.unpack(f"{fmt}{len(row) // 2}H", row))
    for i in range(len(values) - 1, spp - 1, -1):
        values[i] = (values[i] - values[i - spp]) & 0xFFFF
    return struct.pack(f"{fmt}{len(values)}H", *values)


def tiff(w, h, bps, spp, photometric, rows, big=False, compression=1, predictor=1, rps=None,
         tile=None, extra=None, colormap=None):
    """Writes the rows given (packed samples, one bytes object per image row)
    as strips or as tiles, with the data first and the IFD at the end."""
    fmt = ">" if big else "<"
    row_bytes = (w * spp * bps + 7) // 8

    def chunk_rows(x0, cw, y0, ch):
        out = []
        for y in range(y0, y0 + ch):
            if bps % 8:
                assert x0 == 0
                line = rows[y] if y < h else bytes(row_bytes)
            else:
                size = spp * bps // 8
                line = rows[y][x0 * size:(x0 + cw) * size] if y < h else b""
                line += bytes(cw * size - len(line))
            if predictor == 2:
                line = differenced(line, spp, bps, big)
            out.append(line)
        return b"".join(out)

    if tile:
        tw, th = tile
        chunks = [chunk_rows(tx, tw, ty, th) for ty in range(0, h, th) for tx in range(0, w, tw)]
    else:
        rps = rps or h
        chunks = [chunk_rows(0, w, y, min(rps, h - y)) for y in range(0, h, rps)]
    if compression == 8:
        chunks = [zlib.compress(c) for c in chunks]

    data = bytearray(b"MM\x00*" if big else b"II*\x00")
    data += bytes(4)
    offsets = []
    for c in chunks:
        offsets.append(len(data))
        data += c
        data += bytes(len(data) % 2)

    entries = {256: (4, [w]), 257: (4, [h]), 258: (3, [bps] * spp), 259: (3, [compression]),
               262: (3, [photometric]), 277: (3, [spp]), 284: (3, [1])}
    if tile:
        entries.update({322: (3, [tile[0]]), 323: (3, [tile[1]]), 324: (4, offsets),
                        325: (4, [len(c) for c in chunks])})
    else:
        entries.update({273: (4, offsets), 278: (4, [rps]), 279: (4, [len(c) for c in chunks])})
    if predictor != 1:
        entries[317] = (3, [predictor])
    if extra is not None:
        entries[338] = (3, [extra])
    if colormap is not None:
        entries[320] = (3, colormap)

    ifd = len(data)
    data[4:8] = struct.pack(f"{fmt}I", ifd)
    tail = ifd + 2 + 12 * len(entries) + 4
    body = struct.pack(f"{fmt}H", len(entries))
    arrays = b""
    for tag in sorted(entries):
        type_, values = entries[tag]
        packed = struct.pack(f"{fmt}{len(values)}{'H' if type_ == 3 else 'I'}", *values)
        if len(packed) <= 4:
            field = packed + bytes(4 - len(packed))
        else:
            field = struct.pack(f"{fmt}I", tail + len(arrays))
            arrays += packed + bytes(len(packed) % 2)
        body += struct.pack(f"{fmt}HHI", tag, type_, len(values)) + field
    body += bytes(4)
    return bytes(data) + body + arrays


def rgb_rows(w, h, bps, big=False, alpha=False):
    rows = []
    for line in rgb_pattern(w, h):
        values = []
        for x, (r, g, b) in enumerate(line):
            values += [r, g, b] + ([(x * 50 + 30) & 255] if alpha else [])
        if bps == 16:
            values = [v * 256 + 3 for v in values]
            rows.append(struct.pack(f"{'>' if big else '<'}{len(values)}H", *values))
        else:
            rows.append(bytes(values))
    return rows


def premultiplied_rows(w, h):
    rows = []
    for line in rgb_pattern(w, h):
        values = []
        for x, (r, g, b) in enumerate(line):
            a = (x * 50 + 30) & 255
            values += [c * a // 255 for c in (r, g, b)] + [a]
        rows.append(bytes(values))
    return rows


def gray4_rows(w, h):
    return [pack_indices([(x + 3 * y) % 16 for x in range(w)], 4) for y in range(h)]


def checked_by_pillow(data):
    image = Image.open(io.BytesIO(data))
    image.load()
    return image


emit_case("Bmp1", bmp_indexed(13, 3, 1))
emit_case("Bmp4", bmp_indexed(7, 4, 4))
emit_case("Bmp8", bmp_indexed(5, 3, 8, top_down=True))
emit_case("Bmp24", bmp_24(7, 5))
emit_case("Bmp24Os2", bmp_24(6, 3, header=12))
emit_case("Bmp16", bmp_16(5, 3, (0x7C00, 0x03E0, 0x001F)))
emit_case("Bmp565", bmp_16(5, 3, (0xF800, 0x07E0, 0x001F)))
emit_case("Bmp32", bmp_32_rgb(4, 3))
emit_case("Bmp32V5", bmp_32_bitfields(4, 3, 124))
emit_case("BmpRle8", bmp_rle8())
emit_case("BmpRle4", bmp_rle4())

emit_case("Gif", gif_from_pillow(13, 9, interlace=False))
emit_case("GifInterlaced", gif_from_pillow(20, 19, interlace=True))
emit_case("GifTransparent", gif_from_pillow(6, 4, interlace=False, transparency=5))
emit_case("GifAnimated", gif_animated(8, 5))
gif, expected = gif_offset_frame()
emit_case("GifOffset", gif, expected=expected)

emit_case("TiffGray", tiff_from_pillow(pattern_image("L", 11, 9), compression="tiff_lzw",
                                       tiffinfo={278: 4, 317: 2}), mode="L")
emit_case("TiffRgb", tiff_from_pillow(pattern_image("RGB", 9, 7), compression="packbits",
                                      tiffinfo={278: 3}))
emit_case("TiffPalette", tiff_from_pillow(pattern_image("P", 10, 6)))
emit_case("TiffRgba", tiff_from_pillow(pattern_image("RGBA", 7, 5),
                                       compression="tiff_adobe_deflate"))
emit_case("TiffGrayAlpha", tiff_from_pillow(pattern_image("LA", 7, 5), compression="tiff_lzw"),
          mode="L")
emit_case("TiffBilevel", tiff_from_pillow(pattern_image("1", 13, 4)), mode="L")
emit_case("TiffTiled", tiff(37, 21, 8, 3, 2, rgb_rows(37, 21, 8), compression=8, tile=(16, 16)))
emit_case("TiffTiledBig", tiff(37, 21, 8, 3, 2, rgb_rows(37, 21, 8), big=True, tile=(16, 16),
                               predictor=2, compression=8))
emit_case("TiffWhite4", tiff(9, 5, 4, 1, 0, gray4_rows(9, 5), big=True, rps=2), mode="L")

# Pillow rounds 16-bit samples to 8 bits; the decoder keeps the high byte.
data = tiff(8, 6, 16, 3, 2, rgb_rows(8, 6, 16, big=True), big=True, compression=8, predictor=2,
            rps=4)
expected = Image.new("RGB", (8, 6))
expected.putdata([px for line in rgb_pattern(8, 6) for px in line])
assert all(abs(a - b) <= 1 for a, b in zip(checked_by_pillow(data).convert("RGB").tobytes(),
                                           expected.tobytes()))
emit_case("TiffRgb16", data, expected=expected)

data = tiff(6, 4, 8, 4, 2, premultiplied_rows(6, 4), extra=1)
expected = Image.new("RGB", (6, 4))
expected.putdata([tuple(min(255, c * ((x * 50 + 30) & 255) // 255 + 255 - ((x * 50 + 30) & 255))
                        for c in px) for line in rgb_pattern(6, 4) for x, px in enumerate(line)])
checked_by_pillow(data)
emit_case("TiffAssociated", data, expected=expected)
