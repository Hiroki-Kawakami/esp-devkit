#!/usr/bin/env python3
# Writes one small file per container. The payloads are noise: a demuxer never
# looks inside them, and noise has the 0xFF bytes that make its scans work.
import random
import struct
import sys

out = sys.argv[1]
rng = random.Random(1234)


def noise(n):
    return rng.getrandbits(8 * n).to_bytes(n, "little")


def write(name, data):
    with open(f"{out}/{name}", "wb") as f:
        f.write(data)


def wav():
    pcm = noise(600_000)
    fmt = struct.pack("<HHIIHH", 1, 2, 44100, 176400, 4, 16)
    body = b"WAVEfmt " + struct.pack("<I", len(fmt)) + fmt + b"data" + struct.pack("<I", len(pcm)) + pcm
    write("bench.wav", b"RIFF" + struct.pack("<I", len(body)) + body)


def mp3():
    frames = []
    for i in range(1400):
        padding = i % 3 != 0
        header = bytes([0xFF, 0xFB, 0x90 | (0x02 if padding else 0), 0x00])
        frames.append(header + noise(417 + padding - 4))
    write("bench.mp3", b"".join(frames))


def crc8(data):
    crc = 0
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = ((crc << 1) ^ 0x07) & 0xFF if crc & 0x80 else (crc << 1) & 0xFF
    return crc


def utf8_number(value):
    return chr(value).encode()


def flac():
    block = 4096
    frames = 160
    info = struct.pack(">HH", block, block) + b"\0\0\0" + (4200).to_bytes(3, "big")
    packed = (44100 << 44) | (1 << 41) | (15 << 36) | (block * frames)
    info += packed.to_bytes(8, "big") + bytes(16)
    blob = b"fLaC" + bytes([0x80]) + len(info).to_bytes(3, "big") + info
    for i in range(frames):
        header = bytes([0xFF, 0xF8, 0xC9, 0x18]) + utf8_number(i)
        header += bytes([crc8(header)])
        blob += header + noise(3700 + (i * 37) % 300)
    write("bench.flac", blob)


OGG_CRC = []
for i in range(256):
    r = i << 24
    for _ in range(8):
        r = ((r << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if r & 0x80000000 else (r << 1) & 0xFFFFFFFF
    OGG_CRC.append(r)


def ogg_page(sequence, granule, flags, lacing, body):
    header = b"OggS\0" + bytes([flags]) + struct.pack("<qIII", granule, 77, sequence, 0) + bytes([len(lacing)]) + bytes(lacing)
    crc = 0
    for byte in header + body:
        crc = ((crc << 8) & 0xFFFFFFFF) ^ OGG_CRC[((crc >> 24) ^ byte) & 0xFF]
    return header[:22] + struct.pack("<I", crc) + header[26:] + body


def opus():
    head = b"OpusHead\x01\x02" + struct.pack("<HIHB", 312, 48000, 0, 0)
    tags = b"OpusTags" + struct.pack("<I", 4) + b"none" + struct.pack("<I", 0)
    blob = ogg_page(0, 0, 2, [len(head)], head) + ogg_page(1, 0, 0, [len(tags)], tags)
    granule = 0
    for page in range(120):
        lacing, body = [], b""
        for _ in range(50):
            size = 200 + rng.randrange(40)
            lacing.append(size)
            body += bytes([0xFC]) + noise(size - 1)
            granule += 960
        blob += ogg_page(page + 2, granule, 0, lacing, body)
    write("bench.opus", blob)


def vint(value):
    length = 1
    while value >= (1 << (7 * length)) - 1:
        length += 1
    return ((1 << (7 * length)) | value).to_bytes(length, "big")


def element(eid, body):
    return eid + vint(len(body)) + body


def uint(eid, value, length=None):
    return element(eid, value.to_bytes(length or max(1, (value.bit_length() + 7) // 8), "big"))


def mkv():
    video = uint(b"\xD7", 1) + uint(b"\x83", 1) + element(b"\x86", b"V_MPEG4/ISO/AVC") + element(b"\x63\xA2", bytes(32)) + uint(b"\x23\xE3\x83", 40000000) + element(b"\xE0", uint(b"\xB0", 160) + uint(b"\xBA", 120))
    audio = uint(b"\xD7", 2) + uint(b"\x83", 2) + element(b"\x86", b"A_AAC") + element(b"\x63\xA2", b"\x12\x10") + element(b"\xE1", element(b"\xB5", struct.pack(">f", 44100.0)) + uint(b"\x9F", 2))
    front = element(b"\x15\x49\xA9\x66", uint(b"\x2A\xD7\xB1", 1000000)) + element(b"\x16\x54\xAE\x6B", element(b"\xAE", video) + element(b"\xAE", audio))
    clusters, points = b"", []
    for c in range(30):
        body = uint(b"\xE7", c * 500)
        for f in range(12):
            flags = 0x80 if f == 0 else 0
            body += element(b"\xA3", vint(1) + struct.pack(">hB", f * 40, flags) + noise(1800 + f * 20))
            body += element(b"\xA3", vint(2) + struct.pack(">hB", f * 40, 0x80) + noise(300))
        points.append((c * 500, len(clusters)))
        clusters += element(b"\x1F\x43\xB6\x75", body)
    base = len(front)
    cues = b""
    for time, offset in points:
        cues += element(b"\xBB", uint(b"\xB3", time) + element(b"\xB7", uint(b"\xF7", 1) + uint(b"\xF1", base + offset, 4)))
    segment = front + clusters + element(b"\x1C\x53\xBB\x6B", cues)
    ebml = element(b"\x1A\x45\xDF\xA3", element(b"\x42\x82", b"matroska"))
    write("bench.mkv", ebml + b"\x18\x53\x80\x67" + (len(segment) | (1 << 56)).to_bytes(8, "big") + segment)


def box(kind, *parts):
    body = b"".join(parts)
    return struct.pack(">I4s", len(body) + 8, kind) + body


def full(kind, *parts):
    return box(kind, bytes(4), *parts)


def track(handler, stsd, sizes, offsets, delta, timescale, sync=None):
    count = len(sizes)
    stbl = full(b"stsd", struct.pack(">I", 1), stsd)
    stbl += full(b"stts", struct.pack(">III", 1, count, delta))
    if sync:
        stbl += full(b"stss", struct.pack(">I", len(sync)), *[struct.pack(">I", s) for s in sync])
    stbl += full(b"stsc", struct.pack(">IIII", 1, 1, 1, 1))
    stbl += full(b"stsz", struct.pack(">II", 0, count), *[struct.pack(">I", s) for s in sizes])
    stbl += full(b"stco", struct.pack(">I", count), *[struct.pack(">I", o) for o in offsets])
    tkhd = box(b"tkhd", struct.pack(">I", 1), bytes(36), struct.pack(">II", 0x10000, 0), bytes(40))
    mdhd = full(b"mdhd", struct.pack(">IIII", 0, 0, timescale, count * delta), bytes(4))
    hdlr = full(b"hdlr", bytes(4), handler, bytes(13))
    return box(b"trak", tkhd, box(b"mdia", mdhd, hdlr, box(b"minf", box(b"stbl", stbl))))


def mp4():
    ftyp = box(b"ftyp", b"isom", bytes(4), b"isom")
    samples = 500
    video_sizes = [1400 + (i * 13) % 400 for i in range(samples)]
    audio_sizes = [300 + (i * 7) % 60 for i in range(samples)]
    mdat, video_offsets, audio_offsets = b"", [], []
    at = len(ftyp) + 8
    for v, a in zip(video_sizes, audio_sizes):
        video_offsets.append(at + len(mdat))
        mdat += noise(v)
        audio_offsets.append(at + len(mdat))
        mdat += noise(a)
    avcc = box(b"avcC", bytes(30))
    visual = box(b"avc1", bytes(24), struct.pack(">HH", 160, 120), bytes(50), avcc)
    esds = full(b"esds", bytes([3, 25, 0, 1, 0, 4, 17, 0x40, 0x15]), bytes(11), bytes([5, 2, 0x12, 0x10, 6, 1, 2]))
    sound = box(b"mp4a", bytes(8), struct.pack(">HHHHHHI", 0, 0, 0, 2, 16, 0, 44100 << 16), esds)
    sync = list(range(1, samples + 1, 15))
    moov = box(b"moov", full(b"mvhd", struct.pack(">IIII", 0, 0, 1000, 20000), bytes(80)),
               track(b"vide", visual, video_sizes, video_offsets, 40, 1000, sync),
               track(b"soun", sound, audio_sizes, audio_offsets, 882, 22050))
    write("bench.mp4", ftyp + box(b"mdat", mdat) + moov)


def chunk(fourcc, body):
    return fourcc + struct.pack("<I", len(body)) + body + (b"\0" if len(body) & 1 else b"")


def avi():
    strh_v = b"vids" + b"MJPG" + bytes(12) + struct.pack("<IIII", 1, 25, 0, 500) + bytes(24)
    strf_v = struct.pack("<IIIHHI", 40, 160, 120, 1, 24, 0x47504A4D) + bytes(20)
    strh_a = b"auds" + bytes(16) + struct.pack("<IIII", 1, 44100, 0, 0) + bytes(24)
    strf_a = struct.pack("<HHIIHH", 1, 2, 44100, 176400, 4, 16) + bytes(2)
    avih = struct.pack("<II", 40000, 0) + bytes(8) + struct.pack("<II", 500, 0) + struct.pack("<II", 2, 0) + struct.pack("<II", 160, 120) + bytes(16)
    hdrl = chunk(b"avih", avih) + chunk(b"LIST", b"strl" + chunk(b"strh", strh_v) + chunk(b"strf", strf_v)) + chunk(b"LIST", b"strl" + chunk(b"strh", strh_a) + chunk(b"strf", strf_a))
    movi, index = b"movi", b""
    for i in range(500):
        for fourcc, size in ((b"00dc", 1500 + (i * 11) % 300), (b"01wb", 800)):
            index += fourcc + struct.pack("<III", 0x10, len(movi), size)
            movi += chunk(fourcc, noise(size))
    riff = b"AVI " + chunk(b"LIST", b"hdrl" + hdrl) + chunk(b"LIST", movi) + chunk(b"idx1", index)
    write("bench.avi", b"RIFF" + struct.pack("<I", len(riff)) + riff)


for make in (wav, mp3, flac, opus, mkv, mp4, avi):
    make()
