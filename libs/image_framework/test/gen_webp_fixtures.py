#!/usr/bin/env python3
# Generates webp_fixtures.h: small WebP files, each with the RGB pixels Pillow
# decodes it to (alpha composited over white) as the expected output. cwebp
# writes most of them; libvpx (through ffmpeg) writes the VP8 frames with
# several token partitions, which cwebp cannot; the ALPH filters, an offset
# animation frame and a VP8L stream larger than the decoder's window are
# assembled here.
# run.sh runs it as: python3 gen_webp_fixtures.py > webp_fixtures.h
import io
import os
import struct
import subprocess
import tempfile

from PIL import Image


def emit(name, data):
    print(f"static const unsigned char {name}[] = {{")
    for i in range(0, len(data), 12):
        print("    " + "".join(f"{b}," for b in data[i:i + 12]))
    print("};\n")


def composited(image):
    image.load()
    if image.mode in ("RGBA", "LA") or "transparency" in image.info:
        white = Image.new("RGBA", image.size, (255, 255, 255, 255))
        return Image.alpha_composite(white, image.convert("RGBA")).convert("RGB")
    return image.convert("RGB")


def emit_case(name, data, expected=None):
    if expected is None:
        expected = composited(Image.open(io.BytesIO(data)))
    emit(f"k{name}", data)
    emit(f"k{name}Pixels", expected.tobytes())
    print(f"#define k{name}W {expected.width}")
    print(f"#define k{name}H {expected.height}\n")


def pattern(w, h, alpha=False):
    image = Image.new("RGBA", (w, h))
    image.putdata([((x * 40 + y * 7) & 255, (x * 3 + y * 50) & 255, (x * y * 11) & 255,
                    ((x * 60 + y * 20) & 255) if alpha else 255)
                   for y in range(h) for x in range(w)])
    return image


def indexed(w, h, colors):
    image = Image.new("RGB", (w, h))
    image.putdata([((i * 37) & 255, (i * 91) & 255, (i * 13 + 5) & 255)
                   for y in range(h) for x in range(w)
                   for i in [(x * 3 + y * 5 + (x * y) // 7) % colors]])
    return image


def cwebp(image, *options):
    with tempfile.TemporaryDirectory() as tmp:
        src = os.path.join(tmp, "in.png")
        dst = os.path.join(tmp, "out.webp")
        image.save(src)
        subprocess.run(["cwebp", "-quiet", *options, src, "-o", dst], check=True)
        return open(dst, "rb").read()


class BoolDecoder:
    """RFC 6386's boolean decoder, enough to read the frame header back."""

    def __init__(self, data):
        self.data = data
        self.value = (data[0] << 8) | data[1]
        self.pos = 2
        self.range = 255
        self.count = 0

    def bit(self, prob=128):
        split = 1 + (((self.range - 1) * prob) >> 8)
        if self.value >= split << 8:
            self.range -= split
            self.value -= split << 8
            bit = 1
        else:
            self.range = split
            bit = 0
        while self.range < 128:
            self.value <<= 1
            self.range <<= 1
            self.count += 1
            if self.count == 8:
                self.count = 0
                self.value |= self.data[self.pos] if self.pos < len(self.data) else 0
                self.pos += 1
        return bit

    def value_of(self, n):
        return sum(self.bit() << i for i in reversed(range(n)))


def partitions_and_filter(frame):
    d = BoolDecoder(frame[10:])
    d.value_of(2)
    if d.bit():
        update_map, update_data = d.bit(), d.bit()
        if update_data:
            d.bit()
            for bits in [7] * 4 + [6] * 4:
                if d.bit():
                    d.value_of(bits + 1)
        if update_map:
            for _ in range(3):
                if d.bit():
                    d.value_of(8)
    d.bit()
    level = d.value_of(6)
    d.value_of(3)
    if d.bit() and d.bit():
        for _ in range(8):
            if d.bit():
                d.value_of(7)
    return 1 << d.value_of(2), level


def libvpx(w, h, slices, bitrate):
    """A VP8 key frame from libvpx in a RIFF container. It has to use every
    partition and the loop filter, or the case tests less than it claims."""
    with tempfile.TemporaryDirectory() as tmp:
        ivf = os.path.join(tmp, "out.ivf")
        subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi",
                        "-i", f"testsrc2=size={w}x{h}", "-frames:v", "1", "-c:v", "libvpx",
                        "-slices", str(slices), "-b:v", bitrate, "-f", "ivf", ivf], check=True)
        data = open(ivf, "rb").read()
    frame = data[44:44 + struct.unpack("<I", data[32:36])[0]]
    parts, level = partitions_and_filter(frame)
    assert parts == slices and level > 0, (parts, level)
    return riff(chunk(b"VP8 ", frame))


def chunk(tag, payload):
    return tag + struct.pack("<I", len(payload)) + payload + b"\0" * (len(payload) & 1)


def riff(*chunks):
    body = b"WEBP" + b"".join(chunks)
    return b"RIFF" + struct.pack("<I", len(body)) + body


def chunks_of(data):
    out = {}
    pos = 12
    while pos < len(data):
        size = struct.unpack("<I", data[pos + 4:pos + 8])[0]
        out[data[pos:pos + 4]] = data[pos + 8:pos + 8 + size]
        pos += 8 + size + (size & 1)
    return out


def vp8x(flags, w, h):
    return chunk(b"VP8X", struct.pack("<I", flags) + (w - 1).to_bytes(3, "little") +
                 (h - 1).to_bytes(3, "little"))


def alpha_filtered(w, h, values, method):
    """Raw ALPH data run through filter `method` (1 horizontal, 2 vertical,
    3 gradient) the way the decoder undoes it."""
    out = bytearray()
    for y in range(h):
        for x in range(w):
            a = values[y * w + x]
            left = values[y * w + x - 1] if x else None
            top = values[(y - 1) * w + x] if y else None
            if y == 0:
                pred = left if x else 0
            elif method == 1:
                pred = left if x else top
            elif method == 2:
                pred = top
            elif x == 0:
                pred = top
            else:
                pred = max(0, min(255, left + top - values[(y - 1) * w + x - 1]))
            out.append((a - pred) & 255)
    return bytes(out)


def lossy_with_alpha(w, h, method):
    image = pattern(w, h, alpha=True)
    alpha = list(image.getchannel("A").tobytes())
    vp8 = chunks_of(cwebp(image.convert("RGB"), "-q", "80"))[b"VP8 "]
    alph = bytes([method << 2]) + alpha_filtered(w, h, alpha, method)
    return riff(vp8x(0x10, w, h), chunk(b"ALPH", alph), chunk(b"VP8 ", vp8))


def offset_frame():
    """The first frame of an animation, 7x5 at (4, 2) on a 15x11 canvas. The
    canvas around it is white, which is what the decoder shows there."""
    frame = pattern(7, 5).convert("RGB")
    vp8l = chunks_of(cwebp(frame, "-lossless"))[b"VP8L"]
    anmf = (b"".join(v.to_bytes(3, "little") for v in (2, 1, 6, 4, 100)) + b"\0" +
            chunk(b"VP8L", vp8l))
    data = riff(vp8x(0x02, 15, 11), chunk(b"ANIM", bytes(6)), chunk(b"ANMF", anmf))
    expected = Image.new("RGB", (15, 11), (255, 255, 255))
    expected.paste(frame, (4, 2))
    return data, expected


# ---- a VP8L stream past the decoder's window --------------------------------

class Bits:
    def __init__(self):
        self.out = bytearray()
        self.acc = 0
        self.n = 0

    def put(self, value, n):
        self.acc |= value << self.n
        self.n += n
        while self.n >= 8:
            self.out.append(self.acc & 255)
            self.acc >>= 8
            self.n -= 8

    def data(self):
        return bytes(self.out) + (bytes([self.acc]) if self.n else b"")


def canonical(lengths):
    codes = {}
    code = 0
    for length in range(1, 16):
        for s, l in enumerate(lengths):
            if l == length:
                codes[s] = (int(f"{code:0{length}b}"[::-1], 2), length)
                code += 1
        code <<= 1
    return codes


ORDER = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]


def put_code(bits, lengths):
    """Writes a normal prefix code whose lengths use one or two values, each
    sent as its own code-length symbol."""
    used = sorted(set(lengths))
    cl = [0] * 19
    for v in used:
        cl[v] = 1
    bits.put(0, 1)
    num = max(ORDER.index(v) for v in used) + 1
    bits.put(max(num, 4) - 4, 4)
    for i in range(max(num, 4)):
        bits.put(cl[ORDER[i]], 3)
    bits.put(0, 1)
    if len(used) > 1:
        cl_codes = canonical(cl)
        for l in lengths:
            bits.put(*cl_codes[l])
    return canonical(lengths)


def prefix(value):
    if value <= 4:
        return value - 1, 0, 0
    v = value - 1
    high = v.bit_length() - 1
    second = (v >> (high - 1)) & 1
    extra = high - 1
    return 2 * high + second, extra, v & ((1 << extra) - 1)


def far_vp8l(w, h):
    """Row 0 literal; then copies from w - 1 back up to row 950, then from
    nearly 950 rows back, past what the decoder keeps unless it wraps."""
    seed = 1
    row0 = []
    for _ in range(w):
        seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
        row0.append(((seed >> 8) & 255, (seed >> 16) & 255, (seed >> 4) & 255))
    pixels = list(row0)
    plan = []
    total = w * h
    far = 950 * w - 7
    while len(pixels) < total:
        dist = w - 1 if len(pixels) < 950 * w else far
        length = min(4096, total - len(pixels))
        plan.append((length, dist))
        for _ in range(length):
            pixels.append(pixels[len(pixels) - dist])

    bits = Bits()
    bits.put(0x2F, 8)
    bits.put(w - 1, 14)
    bits.put(h - 1, 14)
    bits.put(0, 1)
    bits.put(0, 3)
    bits.put(0, 1)                                   # no transform
    bits.put(0, 1)                                   # no color cache
    bits.put(0, 1)                                   # one prefix code group
    green = put_code(bits, [8] * 232 + [9] * 48)
    red = put_code(bits, [8] * 256)
    blue = put_code(bits, [8] * 256)
    bits.put(1, 1)                                   # alpha: one symbol, 255
    bits.put(0, 1)
    bits.put(1, 1)
    bits.put(255, 8)
    dist_code = put_code(bits, [5] * 24 + [6] * 16)
    for r, g, b in row0:
        bits.put(*green[g])
        bits.put(*red[r])
        bits.put(*blue[b])
    for length, dist in plan:
        sym, n, extra = prefix(length)
        bits.put(*green[256 + sym])
        bits.put(extra, n)
        sym, n, extra = prefix(dist + 120)
        bits.put(*dist_code[sym])
        bits.put(extra, n)

    rgb = bytes(c for p in pixels for c in p)
    image = Image.frombytes("RGB", (w, h), rgb)
    assert composited(Image.open(io.BytesIO(riff(chunk(b"VP8L", bits.data()))))).tobytes() == rgb
    fnv = 0x811C9DC5
    for c in rgb:
        fnv = ((fnv ^ c) * 0x01000193) & 0xFFFFFFFF
    return riff(chunk(b"VP8L", bits.data())), image.size, fnv


print("/* Generated by gen_webp_fixtures.py. */\n")
emit_case("WebpLossy", cwebp(pattern(37, 29), "-q", "75"))
emit_case("WebpLossySimple", cwebp(pattern(37, 29), "-q", "75", "-nostrong", "-f", "80"))
emit_case("WebpLossyNoFilter", cwebp(pattern(37, 29), "-q", "60", "-f", "0", "-segments", "1"))
emit_case("WebpLossyQ100", cwebp(pattern(21, 18), "-q", "100", "-sharpness", "4"))
emit_case("WebpLossyParts", libvpx(16, 128, 8, "60k"))
emit_case("WebpLossless", cwebp(pattern(37, 29), "-lossless"))
emit_case("WebpLosslessZ9", cwebp(pattern(37, 29), "-z", "9"))
emit_case("WebpLosslessAlpha", cwebp(pattern(23, 17, alpha=True), "-lossless", "-exact"))
emit_case("WebpPalette2", cwebp(indexed(29, 9, 2), "-lossless"))
emit_case("WebpPalette4", cwebp(indexed(29, 9, 4), "-lossless"))
emit_case("WebpPalette16", cwebp(indexed(29, 9, 16), "-lossless"))
emit_case("WebpPalette200", cwebp(indexed(29, 9, 200), "-lossless"))
emit_case("WebpAlpha", cwebp(pattern(37, 29, alpha=True), "-q", "70"))
emit_case("WebpAlphaHorizontal", lossy_with_alpha(19, 13, 1))
emit_case("WebpAlphaVertical", lossy_with_alpha(19, 13, 2))
emit_case("WebpAlphaGradient", lossy_with_alpha(19, 13, 3))

frames = [pattern(16, 12, alpha=True), pattern(16, 12)]
buf = io.BytesIO()
frames[0].save(buf, "WEBP", save_all=True, append_images=frames[1:], lossless=True, duration=50)
emit_case("WebpAnimated", buf.getvalue())
data, expected = offset_frame()
emit_case("WebpAnimOffset", data, expected=expected)

data, (w, h), fnv = far_vp8l(1100, 1000)
emit("kWebpFar", data)
print(f"#define kWebpFarW {w}")
print(f"#define kWebpFarH {h}")
print(f"#define kWebpFarHash 0x{fnv:08x}u")
