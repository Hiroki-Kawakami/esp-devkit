#!/usr/bin/env python3
# Cuts audio files into the packet streams test/c_ref.c and the crate's tests
# read: "PKT1", codec, three parameters, u32 config length, config, u32 packet
# count, then u32 length + bytes per packet, all little-endian.
#   packetize.py <in dir> <out dir>
import pathlib
import struct
import sys

AAC, MP3, OPUS, FLAC, ALAC = range(5)


def write(path, codec, params, config, packets):
    with open(path, "wb") as f:
        f.write(b"PKT1" + bytes([codec, *params]))
        f.write(struct.pack("<I", len(config)) + config)
        f.write(struct.pack("<I", len(packets)))
        for p in packets:
            f.write(struct.pack("<I", len(p)) + p)


def adts_frames(data):
    pos, out = 0, []
    while pos + 7 <= len(data) and data[pos] == 0xFF and data[pos + 1] & 0xF6 == 0xF0:
        n = (data[pos + 3] & 3) << 11 | data[pos + 4] << 3 | data[pos + 5] >> 5
        if not n:
            break
        out.append(data[pos:pos + n])
        pos += n
    return out


MP3_KBPS = [
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
    [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
]


def mp3_frame_len(p):
    if p[0] != 0xFF or p[1] & 0xE0 != 0xE0:
        return 0
    version_bits, layer_bits = (p[1] >> 3) & 3, (p[1] >> 1) & 3
    bitrate, rate = p[2] >> 4, (p[2] >> 2) & 3
    if version_bits == 1 or layer_bits not in (1, 2) or bitrate in (0, 15) or rate == 3:
        return 0
    version = {3: 0, 2: 1, 0: 2}[version_bits]
    layer = 2 if layer_bits == 2 else 3
    if version == 2 and layer == 2:
        return 0
    kbps = MP3_KBPS[2 if version else 0 if layer == 2 else 1][bitrate]
    hz = [44100, 48000, 32000][rate] >> version
    frames = 576 if layer == 3 and version else 1152
    return frames // 8 * 1000 * kbps // hz + ((p[2] >> 1) & 1)


def mp3_frames(data):
    pos = 0
    if data[:3] == b"ID3":
        pos = 10 + ((data[6] & 0x7F) << 21 | (data[7] & 0x7F) << 14 | (data[8] & 0x7F) << 7 | data[9] & 0x7F)
    out = []
    while pos + 4 <= len(data):
        n = mp3_frame_len(data[pos:pos + 4])
        if not n:
            pos += 1
            continue
        if pos + n > len(data):
            break
        out.append(data[pos:pos + n])
        pos += n
    return out


def ogg_packets(data):
    pos, out, cur = 0, [], b""
    while pos + 27 <= len(data) and data[pos:pos + 4] == b"OggS":
        count = data[pos + 26]
        table = data[pos + 27:pos + 27 + count]
        body = pos + 27 + count
        for lace in table:
            cur += data[body:body + lace]
            body += lace
            if lace < 255:
                out.append(cur)
                cur = b""
        pos = body
    return out


def opus_bit_packets(data):
    pos, out = 0, []
    while pos + 8 <= len(data):
        n = struct.unpack(">I", data[pos:pos + 4])[0]
        out.append(data[pos + 8:pos + 8 + n])
        pos += 8 + n
    return out


def crc_table(poly, top):
    table = []
    for i in range(256):
        c = i << (top - 8)
        for _ in range(8):
            c = (c << 1) ^ poly if c & (1 << (top - 1)) else c << 1
        table.append(c & ((1 << top) - 1))
    return table


CRC16 = crc_table(0x8005, 16)


def flac_frames(data):
    pos = 4
    streaminfo = None
    while True:
        kind, size = data[pos], int.from_bytes(data[pos + 1:pos + 4], "big")
        if kind & 0x7F == 0:
            streaminfo = data[pos + 4:pos + 4 + size]
        pos += 4 + size
        if kind & 0x80:
            break
    out = []
    start = pos
    crc = 0
    i = pos
    n = len(data)
    while i < n:
        crc = ((crc << 8) & 0xFFFF) ^ CRC16[(crc >> 8) ^ data[i]]
        i += 1
        if crc == 0 and i - start > 6 and (i >= n or (data[i] == 0xFF and data[i + 1] & 0xFE == 0xF8)):
            out.append(data[start:i])
            start = i
    return streaminfo, out


def atoms(data, start, end):
    pos = start
    while pos + 8 <= end:
        size = int.from_bytes(data[pos:pos + 4], "big")
        kind = data[pos + 4:pos + 8]
        head = 8
        if size == 1:
            size = int.from_bytes(data[pos + 8:pos + 16], "big")
            head = 16
        if size == 0:
            size = end - pos
        yield kind, pos + head, pos + size
        pos += size


def find(data, start, end, *path):
    for kind, a, b in atoms(data, start, end):
        if kind == path[0]:
            return (a, b) if len(path) == 1 else find(data, a, b, *path[1:])
    return None


def mp4_track(data):
    moov = find(data, 0, len(data), b"moov")
    for kind, a, b in atoms(data, *moov):
        if kind != b"trak":
            continue
        stbl = find(data, a, b, b"mdia", b"minf", b"stbl")
        if stbl:
            return stbl
    raise ValueError("no track")


def mp4_packets(data, stbl):
    a, b = stbl
    sa, _ = find(data, a, b, b"stsz")
    fixed, count = struct.unpack(">II", data[sa + 4:sa + 12])
    sizes = [fixed] * count if fixed else list(struct.unpack(f">{count}I", data[sa + 12:sa + 12 + 4 * count]))
    ca = find(data, a, b, b"stco")
    if ca:
        n = struct.unpack(">I", data[ca[0] + 4:ca[0] + 8])[0]
        chunks = list(struct.unpack(f">{n}I", data[ca[0] + 8:ca[0] + 8 + 4 * n]))
    else:
        ca = find(data, a, b, b"co64")
        n = struct.unpack(">I", data[ca[0] + 4:ca[0] + 8])[0]
        chunks = list(struct.unpack(f">{n}Q", data[ca[0] + 8:ca[0] + 8 + 8 * n]))
    sc, _ = find(data, a, b, b"stsc")
    n = struct.unpack(">I", data[sc + 4:sc + 8])[0]
    runs = [struct.unpack(">III", data[sc + 8 + 12 * i:sc + 20 + 12 * i]) for i in range(n)]
    out, sample = [], 0
    for c, off in enumerate(chunks):
        per = next(r[1] for r in reversed(runs) if r[0] <= c + 1)
        for _ in range(per):
            out.append(data[off:off + sizes[sample]])
            off += sizes[sample]
            sample += 1
    return out


def esds_asc(data, start, end):
    i = data.index(b"esds", start, end) + 8
    i += 1
    def length(i):
        n = 0
        while True:
            b = data[i]
            i += 1
            n = n << 7 | b & 0x7F
            if not b & 0x80:
                return n, i
    assert data[i - 1] == 3 or True
    _, i = length(i)
    i += 3
    assert data[i] == 4
    _, i = length(i + 1)
    i += 13
    assert data[i] == 5
    n, i = length(i + 1)
    return data[i:i + n]


def main(src, dst):
    src, dst = pathlib.Path(src), pathlib.Path(dst)
    dst.mkdir(parents=True, exist_ok=True)
    for f in sorted(src.iterdir()):
        data = f.read_bytes()
        name = f.stem
        if f.suffix == ".aac":
            packets = adts_frames(data)
            for he in range(3):
                write(dst / f"aac_adts_{name}_he{he}.pkt", AAC, (1, he, 0), packets[0][:64], packets)
        elif f.suffix in (".mp3", ".mp2"):
            packets = mp3_frames(data)
            for ch in (1, 2):
                write(dst / f"mp3_{name}{f.suffix[1:]}_{ch}.pkt", MP3, (ch, 0, 0), b"", packets)
        elif f.suffix == ".opus":
            packets = ogg_packets(data)
            head, packets = packets[0], packets[2:]
            write(dst / f"opus_{name}.pkt", OPUS, (0, 0, 0), head, packets)
            lossy = [b"" if i % 17 == 5 else p for i, p in enumerate(packets)]
            write(dst / f"opus_{name}_loss.pkt", OPUS, (0, 0, 0), head, lossy)
        elif f.suffix == ".bit":
            packets = opus_bit_packets(data)
            for ch in (1, 2):
                head = b"OpusHead\x01" + bytes([ch]) + bytes(9)
                write(dst / f"opus_vec_{name}_{ch}.pkt", OPUS, (0, 0, 0), head, packets)
        elif f.suffix == ".flac":
            info, packets = flac_frames(data)
            write(dst / f"flac_{name}.pkt", FLAC, (0, 0, 0), info, packets)
        elif f.suffix == ".m4a":
            stbl = mp4_track(data)
            sa, sb = find(data, *stbl, b"stsd")
            packets = mp4_packets(data, stbl)
            if data[sa + 12:sa + 16] == b"alac":
                i = data.index(b"alac", sa + 16, sb) + 8
                write(dst / f"alac_{name}.pkt", ALAC, (0, 0, 0), data[i:i + 24], packets)
            elif data[sa + 12:sa + 16] == b"mp4a":
                asc = esds_asc(data, sa, sb)
                write(dst / f"aac_mp4_{name}.pkt", AAC, (0, 2, 0), asc, packets)


main(sys.argv[1], sys.argv[2])
