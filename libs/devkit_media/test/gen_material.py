#!/usr/bin/env python3
# Writes the files the differential test runs over into a directory. ffmpeg,
# lame, flac and opusenc come from nix-shell (test/run.sh).
import os
import random
import struct
import subprocess
import sys
import zlib

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
SECONDS = "6"
VIDEO = ["-f", "lavfi", "-i", f"testsrc2=size=160x120:rate=15:duration={SECONDS}"]
LARGE = ["-f", "lavfi", "-i", "testsrc2=size=640x480:rate=10:duration=4"]
AUDIO = ["-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=44100:duration={SECONDS},aeval=val(0)+random(0)*0.02:c=stereo"]
TAGS = ["-metadata", "title=Synthetic title", "-metadata", "artist=Nobody", "-metadata", "album=Test album",
        "-metadata", "album_artist=Nobody else", "-metadata", "date=2001", "-metadata", "track=3/9"]


def path(name):
    return os.path.join(out, name)


def run(*args):
    subprocess.run(args, check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def ffmpeg(name, *args):
    try:
        run("ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-fflags", "+bitexact", *args, path(name))
    except subprocess.CalledProcessError as e:
        print(f"skipped {name}: {e.stderr.decode()[-200:]}")


ffmpeg("cover.jpg", "-f", "lavfi", "-i", "testsrc2=size=64x64:duration=1", "-frames:v", "1", "-update", "1")
ffmpeg("cover.png", "-f", "lavfi", "-i", "testsrc2=size=48x48:duration=1", "-frames:v", "1", "-update", "1")
ffmpeg("large_cover.jpg", "-f", "lavfi", "-i", "testsrc2=size=1024x1024:duration=1", "-frames:v", "1", "-update", "1", "-q:v", "1")
COVER = open(path("cover.jpg"), "rb").read()
COVER_PNG = open(path("cover.png"), "rb").read()
LARGE_COVER = open(path("large_cover.jpg"), "rb").read()

X264 = ["-c:v", "libx264", "-preset", "ultrafast", "-g", "15", "-pix_fmt", "yuv420p"]
X264_B = ["-c:v", "libx264", "-preset", "medium", "-g", "15", "-bf", "2", "-pix_fmt", "yuv420p"]
MJPEG = ["-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v", "5"]
MPEG2 = ["-c:v", "mpeg2video", "-g", "15", "-bf", "2"]

# AVI
ffmpeg("mjpeg_pcm.avi", *VIDEO, *AUDIO, *MJPEG, "-c:a", "pcm_s16le", *TAGS)
ffmpeg("h264_mp3.avi", *VIDEO, *AUDIO, *X264, "-c:a", "libmp3lame", "-b:a", "96k")
ffmpeg("h264_global.avi", *VIDEO, *X264, "-flags", "+global_header")
ffmpeg("mpeg2_aac.avi", *VIDEO, *AUDIO, *MPEG2, "-c:a", "aac", "-b:a", "96k")
ffmpeg("audio_first.avi", *VIDEO, *AUDIO, *MJPEG, "-c:a", "pcm_s16le", "-map", "1:a", "-map", "0:v")
ffmpeg("adpcm.avi", *VIDEO, *AUDIO, *MJPEG, "-c:a", "adpcm_ima_wav")
ffmpeg("mpeg4.avi", *VIDEO, "-c:v", "mpeg4")
ffmpeg("large_mjpeg.avi", *LARGE, "-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v", "2")


def strip_idx1(src, dst):
    data = bytearray(open(path(src), "rb").read())
    at = data.rfind(b"idx1")
    if at > 0:
        data[at : at + 4] = b"JUNK"
    open(path(dst), "wb").write(data)


def absolute_idx1(src, dst):
    data = bytearray(open(path(src), "rb").read())
    at = data.rfind(b"idx1")
    size = struct.unpack_from("<I", data, at + 4)[0]
    movi = data.find(b"movi")
    for entry in range(at + 8, at + 8 + size, 16):
        offset = struct.unpack_from("<I", data, entry + 8)[0]
        struct.pack_into("<I", data, entry + 8, offset + movi)
    open(path(dst), "wb").write(data)


def bad_idx1(src, dst):
    data = bytearray(open(path(src), "rb").read())
    at = data.rfind(b"idx1")
    size = struct.unpack_from("<I", data, at + 4)[0]
    for entry in range(at + 8, at + 8 + size, 16):
        struct.pack_into("<I", data, entry + 8, 3)
    open(path(dst), "wb").write(data)


strip_idx1("h264_mp3.avi", "h264_noindex.avi")
strip_idx1("mpeg2_aac.avi", "mpeg2_noindex.avi")
strip_idx1("mjpeg_pcm.avi", "mjpeg_noindex.avi")
absolute_idx1("mjpeg_pcm.avi", "absolute_idx1.avi")
bad_idx1("mjpeg_pcm.avi", "bad_idx1.avi")

# WAV
ffmpeg("s16.wav", *AUDIO, "-c:a", "pcm_s16le", *TAGS)
ffmpeg("s24.wav", *AUDIO, "-c:a", "pcm_s24le")
ffmpeg("s32.wav", *AUDIO, "-c:a", "pcm_s32le")
ffmpeg("u8.wav", *AUDIO, "-c:a", "pcm_u8")
ffmpeg("f32.wav", *AUDIO, "-c:a", "pcm_f32le")
ffmpeg("ima.wav", *AUDIO, "-c:a", "adpcm_ima_wav")
ffmpeg("mp3_in.wav", *AUDIO, "-c:a", "libmp3lame")
ffmpeg("extensible.wav", *AUDIO, "-ac", "6", "-c:a", "pcm_s16le", "-rf64", "never")


def chunk(fourcc, body):
    return fourcc + struct.pack("<I", len(body)) + body + (b"\0" if len(body) & 1 else b"")


def wav_variant(name, body_size=None, after=b"", before=b"", truncate=0):
    fmt = struct.pack("<HHIIHH", 1, 2, 44100, 176400, 4, 16)
    pcm = bytes(random.Random(7).getrandbits(8) for _ in range(200000))
    data = b"data" + struct.pack("<I", len(pcm) if body_size is None else body_size) + pcm
    riff = b"WAVE" + chunk(b"fmt ", fmt) + before + data + after
    blob = b"RIFF" + struct.pack("<I", len(riff)) + riff
    open(path(name), "wb").write(blob[: len(blob) - truncate])


INFO = b"INFO" + chunk(b"INAM", b"Info title") + chunk(b"IART", b"Info artist") + chunk(b"ICRD", b"1999") + chunk(b"ITRK", b"7") + chunk(b"IPRD", b"Info album\0")
ID3_TAG = None
wav_variant("streaming_zero.wav", body_size=0)
wav_variant("streaming_big.wav", body_size=0x7FFFFFFF)
wav_variant("list_after.wav", after=chunk(b"LIST", INFO))
wav_variant("list_before.wav", before=chunk(b"LIST", INFO))
wav_variant("cut.wav", truncate=1001)

# Elementary streams
ffmpeg("cbr.mp3", *AUDIO, "-c:a", "libmp3lame", "-b:a", "128k", "-write_xing", "0", "-id3v2_version", "0")
ffmpeg("xing.mp3", *AUDIO, "-c:a", "libmp3lame", "-q:a", "2", *TAGS)
ffmpeg("mono.mp3", *AUDIO, "-ac", "1", "-c:a", "libmp3lame", "-b:a", "64k", "-id3v2_version", "3", *TAGS)
ffmpeg("lsf.mp3", *AUDIO, "-ar", "22050", "-c:a", "libmp3lame", "-b:a", "48k", "-write_xing", "0")
ffmpeg("l2.mp2", *AUDIO, "-c:a", "mp2", "-b:a", "192k")
ffmpeg("adts.aac", *AUDIO, "-c:a", "aac", "-b:a", "96k", "-f", "adts")
ffmpeg("cover_mp3.mp3", *AUDIO, "-i", path("cover.jpg"), "-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-b:a", "64k", "-c:v", "copy", "-id3v2_version", "3", "-disposition:v", "attached_pic", *TAGS)
ffmpeg("cover_v4.mp3", *AUDIO, "-i", path("cover.png"), "-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-b:a", "64k", "-c:v", "copy", "-id3v2_version", "4", "-disposition:v", "attached_pic", *TAGS)
try:
    run("lame", "--quiet", "-V", "4", "--ti", path("cover.jpg"), "--tt", "Lame title", "--ta", "Lame artist", "--tl", "Lame album", "--ty", "2003", "--tn", "2/5", path("s16.wav"), path("lame_vbr.mp3"))
    run("lame", "--quiet", "-b", "128", "--id3v1-only", "--tt", "V1 title", "--ta", "V1 artist", "--tn", "4", path("s16.wav"), path("lame_v1.mp3"))
    run("lame", "--quiet", "-b", "96", "--add-id3v2", "--tt", "Both title", "--ty", "2005", path("s16.wav"), path("lame_both.mp3"))
except subprocess.CalledProcessError as e:
    print("lame:", e.stderr[-200:])

mp3 = open(path("cbr.mp3"), "rb").read()


def synchsafe(value):
    return bytes((value >> s) & 0x7F for s in (21, 14, 7, 0))


def id3_text(fid, text, major, encoding=3):
    raw = (lambda: text.encode("latin-1"), lambda: b"\xff\xfe" + text.encode("utf-16-le"), lambda: text.encode("utf-16-be"), text.encode)[encoding]()
    body = bytes([encoding]) + raw
    size = synchsafe(len(body)) if major == 4 else struct.pack(">I", len(body))
    return fid + size + b"\0\0" + body


def id3(frames, major=3, flags=0):
    return b"ID3" + bytes([major, 0, flags]) + synchsafe(len(frames)) + frames


ape_body = b"\0" * 40
ape = b"APETAGEX" + struct.pack("<IIII", 2000, len(ape_body) + 32, 1, 0xA0000000) + b"\0" * 8
ape_header = b"APETAGEX" + struct.pack("<IIII", 2000, len(ape_body) + 32, 1, 0xE0000000) + b"\0" * 8
open(path("ape.mp3"), "wb").write(mp3 + ape_header + ape_body + ape)
v1 = b"TAG" + b"V1 only".ljust(30, b"\0") + b"A".ljust(30, b"\0") + b"B".ljust(30, b"\0") + b"1980" + b"\0" * 28 + b"\0\x05\xff"
open(path("v1_ape.mp3"), "wb").write(mp3 + ape_body + ape + v1)
open(path("garbage_front.mp3"), "wb").write(bytes(random.Random(1).getrandbits(8) for _ in range(3000)) + mp3)
open(path("gap.mp3"), "wb").write(mp3[:30000] + bytes(random.Random(2).getrandbits(8) for _ in range(5000)) + mp3[30000:])
open(path("id3_utf16.mp3"), "wb").write(id3(id3_text(b"TIT2", "Wide タイトル \U0001F3B5", 3, 1) + id3_text(b"TPE1", "  Spaces  ", 3, 0) + id3_text(b"TALB", "Be", 3, 2) + id3_text(b"TRCK", "1", 3, 0) + id3_text(b"TYER", "1995", 3, 0)) + mp3)
open(path("id3_v4_utf8.mp3"), "wb").write(id3(id3_text(b"TIT2", "Utf8 é", 4, 3) + id3_text(b"TDRC", "2020-01-02", 4, 3), major=4) + mp3)
v22 = b"TT2" + (5).to_bytes(3, "big") + b"\0Name" + b"TP1" + (4).to_bytes(3, "big") + b"\0Art"
pic = b"PIC" + (len(COVER) + 6).to_bytes(3, "big") + b"\0JPG\x03\0" + COVER
open(path("id3_v22.mp3"), "wb").write(id3(v22 + pic, major=2) + mp3)
apic0 = b"\0image/jpeg\0\x00\0" + COVER
apic3 = b"\0image/png\0\x03\0" + COVER_PNG
apics = b"APIC" + struct.pack(">I", len(apic0)) + b"\0\0" + apic0 + b"APIC" + struct.pack(">I", len(apic3)) + b"\0\0" + apic3
open(path("id3_two_pics.mp3"), "wb").write(id3(apics) + mp3)
nonpic = b"\0text/plain\0\x03\0" + b"not a picture at all"
open(path("id3_not_picture.mp3"), "wb").write(id3(b"APIC" + struct.pack(">I", len(nonpic)) + b"\0\0" + nonpic) + mp3)
unsync_frames = id3_text(b"TIT2", "Unsync", 3, 0).replace(b"\xff", b"\xff\0")
open(path("id3_unsync.mp3"), "wb").write(id3(unsync_frames + b"APIC" + struct.pack(">I", len(apic0)) + b"\0\0" + apic0.replace(b"\xff", b"\xff\0"), flags=0x80) + mp3)
open(path("id3_extended.mp3"), "wb").write(id3(struct.pack(">I", 6) + b"\0\0\0\0\0\0" + id3_text(b"TIT2", "Extended", 3, 0), flags=0x40) + mp3)
v4_flags = b"TIT2" + synchsafe(11) + b"\x00\x02" + b"\x00Unsync\xff\x00ab"
open(path("id3_v4_frame_unsync.mp3"), "wb").write(id3(v4_flags, major=4) + mp3)
open(path("id3_footer.mp3"), "wb").write(id3(id3_text(b"TIT2", "Footer", 4, 3), major=4, flags=0x10) + b"3DI\x04\0\x10\0\0\0\0" + mp3)
open(path("id3_only.mp3"), "wb").write(id3(id3_text(b"TIT2", "Only", 3, 0)))

adts = open(path("adts.aac"), "rb").read()
open(path("id3_adts.aac"), "wb").write(id3(id3_text(b"TIT2", "Adts", 3, 0)) + adts)

# FLAC
try:
    run("flac", "--silent", "-f", "-5", "-T", "TITLE=Flac title", "-T", "ARTIST=Flac artist", "-T", "ALBUM=Flac album", "-T", "ALBUMARTIST=Flac album artist", "-T", "DATE=2011", "-T", "TRACKNUMBER=4", "-T", "TRACKTOTAL=12", "-o", path("tagged.flac"), path("s16.wav"))
    run("flac", "--silent", "-f", "-0", "--no-seektable", "-o", path("noseek.flac"), path("s16.wav"))
    run("flac", "--silent", "-f", "-8", "-b", "1024", "-S", "1s", "-o", path("seek1s.flac"), path("s24.wav"))
    run("flac", "--silent", "-f", "--picture", path("cover.jpg"), "-o", path("cover.flac"), path("s16.wav"))
    run("flac", "--silent", "-f", "--picture", "3||||" + path("cover.png"), "--picture", "4||||" + path("cover.jpg"), "-o", path("two_covers.flac"), path("s16.wav"))
except subprocess.CalledProcessError as e:
    print("flac:", e.stderr[-200:])
ffmpeg("ffmpeg.flac", *AUDIO, "-c:a", "flac", *TAGS)
ffmpeg("ffmpeg_var.flac", *AUDIO, "-c:a", "flac", "-compression_level", "12", "-min_prediction_order", "1")

noseek = bytearray(open(path("noseek.flac"), "rb").read())
unknown = bytearray(noseek)
unknown[8 + 13] &= 0xF0
unknown[8 + 14 : 8 + 18] = b"\0\0\0\0"
open(path("unknown_length.flac"), "wb").write(unknown)
zero_frame = bytearray(noseek)
zero_frame[8 + 7 : 8 + 10] = b"\0\0\0"
open(path("no_max_frame.flac"), "wb").write(zero_frame)
wrong_frame = bytearray(noseek)
wrong_frame[8 + 7 : 8 + 10] = b"\0\0\x20"
open(path("wrong_max_frame.flac"), "wb").write(wrong_frame)
open(path("id3_front.flac"), "wb").write(id3(id3_text(b"TIT2", "Front", 3, 0)) + bytes(noseek))
open(path("id3_v1_tail.flac"), "wb").write(bytes(noseek) + v1)
broken = bytearray(noseek)
broken[60000:60040] = bytes(40)
open(path("frame_hole.flac"), "wb").write(broken)

# Ogg Opus
ffmpeg("plain.opus", *AUDIO, "-ar", "48000", "-c:a", "libopus", "-b:a", "64k", *TAGS)
ffmpeg("mono.opus", *AUDIO, "-ac", "1", "-ar", "48000", "-c:a", "libopus", "-b:a", "32k", "-frame_duration", "60")
ffmpeg("vorbis.opus", *AUDIO, "-c:a", "flac", "-f", "ogg")
try:
    run("opusenc", "--quiet", "--title", "Opus title", "--artist", "Opus artist", "--album", "Opus album", "--date", "2015", "--tracknumber", "2", "--picture", path("large_cover.jpg"), path("s16.wav"), path("cover.opus"))
    run("opusenc", "--quiet", "--bitrate", "24", path("s16.wav"), path("plain2.opus"))
except subprocess.CalledProcessError as e:
    print("opusenc:", e.stderr[-200:])

OGG_CRC = []
for i in range(256):
    r = i << 24
    for _ in range(8):
        r = ((r << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if r & 0x80000000 else (r << 1) & 0xFFFFFFFF
    OGG_CRC.append(r)


def ogg_page(serial, sequence, granule, flags, lacing, body):
    header = b"OggS\0" + bytes([flags]) + struct.pack("<qIII", granule, serial, sequence, 0) + bytes([len(lacing)]) + bytes(lacing)
    crc = 0
    for byte in header + body:
        crc = ((crc << 8) & 0xFFFFFFFF) ^ OGG_CRC[((crc >> 24) ^ byte) & 0xFF]
    return header[:22] + struct.pack("<I", crc) + header[26:] + body


def ogg_pages(data):
    at = 0
    pages = []
    while at < len(data):
        count = data[at + 26]
        lacing = list(data[at + 27 : at + 27 + count])
        size = sum(lacing)
        pages.append((data[at + 5], struct.unpack_from("<q", data, at + 6)[0], struct.unpack_from("<I", data, at + 14)[0], lacing, data[at + 27 + count : at + 27 + count + size]))
        at += 27 + count + size
    return pages


def ogg_packets(pages):
    packets, pending, ended = [], b"", []
    for flags, granule, serial, lacing, body in pages:
        at = 0
        for value in lacing:
            pending += body[at : at + value]
            at += value
            if value < 255:
                packets.append(pending)
                pending = b""
        if packets:
            ended.append((len(packets), granule))
    return packets, ended


# Page breaks in the middle of packets, so that packets span pages.
def respan(src, dst, stride):
    pages = ogg_pages(open(path(src), "rb").read())
    head = pages[:2]
    packets, ended = ogg_packets(pages[2:])
    serial = pages[0][2]
    blob = b"".join(ogg_page(serial, i, g, f, l, b) for i, (f, g, s, l, b) in enumerate(head))
    stream = b"".join(packets)
    lacing = []
    for packet in packets:
        lacing += [255] * (len(packet) // 255) + [len(packet) % 255]
    sequence, at, seg, continued = 2, 0, 0, False
    cumulative = {}
    done = 0
    for count, granule in ended:
        cumulative[count] = granule
    finished = 0
    while seg < len(lacing):
        part = lacing[seg : seg + stride]
        size = sum(part)
        finished += sum(1 for v in part if v < 255)
        granule = -1
        for count in sorted(cumulative):
            if count <= finished:
                granule = cumulative[count]
        last = seg + stride >= len(lacing)
        flags = (1 if continued else 0) | (4 if last else 0)
        blob += ogg_page(serial, sequence, granule, flags, part, stream[at : at + size])
        continued = part[-1] == 255
        at += size
        seg += stride
        sequence += 1
    open(path(dst), "wb").write(blob)


ffmpeg("loud.opus", *AUDIO, "-ar", "48000", "-c:a", "libopus", "-b:a", "320k", "-vbr", "off")
respan("loud.opus", "spanning.opus", 2)
respan("loud.opus", "spanning3.opus", 3)
respan("loud.opus", "spanning1.opus", 1)


# A page that does not continue the packet the one before it left open.
def cut_short(src, dst, start):
    pages = ogg_pages(open(path(src), "rb").read())
    index = next(i for i in range(start, len(pages)) if pages[i][0] & 1)
    blob = b""
    for i, (f, g, s, l, b) in enumerate(pages):
        blob += ogg_page(s, i, g, f & ~1 if i == index else f, l, b)
    open(path(dst), "wb").write(blob)


cut_short("spanning.opus", "cut_short.opus", 40)


def extra_stream(src, dst):
    pages = ogg_pages(open(path(src), "rb").read())
    blob = b""
    other = 0x12345678
    for i, (f, g, s, l, b) in enumerate(pages):
        blob += ogg_page(s, i, g, f, l, b)
        if i >= 2 and i % 3 == 0:
            blob += ogg_page(other, i, 0, 0, [16], b"x" * 16)
    open(path(dst), "wb").write(blob)


extra_stream("plain.opus", "multiplexed.opus")
plain = open(path("plain.opus"), "rb").read()
open(path("chained.opus"), "wb").write(plain + open(path("plain2.opus"), "rb").read())
stomped = bytearray(plain)
stomped[20000:20020] = bytes(20)
open(path("bad_crc.opus"), "wb").write(stomped)

# Matroska
ffmpeg("h264_aac.mkv", *VIDEO, *AUDIO, *X264_B, "-c:a", "aac", "-b:a", "96k", *TAGS)
ffmpeg("mjpeg_pcm.mkv", *VIDEO, *AUDIO, *MJPEG, "-c:a", "pcm_s16le")
ffmpeg("mpeg2_mp3.mkv", *VIDEO, *AUDIO, *MPEG2, "-c:a", "libmp3lame")
ffmpeg("h264_mp2.mkv", *VIDEO, *AUDIO, *X264, "-c:a", "mp2")
ffmpeg("h264_opus.mkv", *VIDEO, *AUDIO, *X264, "-ar", "48000", "-c:a", "libopus")
ffmpeg("h264_flac.mkv", *VIDEO, *AUDIO, *X264, "-c:a", "flac")
ffmpeg("h264_alac.mkv", *VIDEO, *AUDIO, *X264, "-c:a", "alac")
ffmpeg("h264_ima.mkv", *VIDEO, *AUDIO, *X264, "-c:a", "adpcm_ima_wav")
ffmpeg("h264_ac3.mkv", *VIDEO, *AUDIO, *X264, "-c:a", "ac3")
ffmpeg("rotated.mkv", "-display_rotation", "90", "-i", path("h264_aac.mkv"), "-c", "copy")
ffmpeg("rotated180.mkv", "-display_rotation", "180", "-i", path("h264_aac.mkv"), "-c", "copy")
ffmpeg("rotated45.mkv", "-display_rotation", "45", "-i", path("h264_aac.mkv"), "-c", "copy")
ffmpeg("video_only.mkv", *VIDEO, *X264)
ffmpeg("live.mkv", *VIDEO, *AUDIO, *X264, "-c:a", "aac", "-live", "1")
ffmpeg("no_cues.mkv", *VIDEO, *X264, "-cues_to_front", "0", "-write_crc32", "0")
ffmpeg("attachment.mkv", *VIDEO, *X264, "-attach", path("cover.jpg"), "-metadata:s:t:0", "mimetype=image/jpeg", "-metadata:s:t:0", "filename=cover.jpg", *TAGS)
ffmpeg("attachment2.mkv", *VIDEO, *X264, "-attach", path("cover.png"), "-metadata:s:t:0", "mimetype=image/png", "-metadata:s:t:0", "filename=other.png", "-attach", path("cover.jpg"), "-metadata:s:t:1", "mimetype=image/jpeg", "-metadata:s:t:1", "filename=Cover.jpg")
ffmpeg("vfr.mkv", *VIDEO, *AUDIO, *MJPEG, "-c:a", "aac", "-vf", "select='not(mod(n\\,3))'", "-fps_mode", "vfr")
ffmpeg("vp9.webm", *VIDEO, "-c:v", "libvpx-vp9", "-cpu-used", "8", "-deadline", "realtime")
ffmpeg("audio_only.mkv", *AUDIO, "-c:a", "flac")
ffmpeg("large_mjpeg.mkv", *LARGE, "-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v", "2")


# Matroska features ffmpeg does not write. The payloads are noise: a demuxer
# never looks inside them.
def vint(value, length=None):
    if length is None:
        length = 1
        while value >= (1 << (7 * length)) - 1:
            length += 1
    return ((1 << (7 * length)) | value).to_bytes(length, "big")


def element(eid, body):
    return eid + vint(len(body)) + body


def uint(eid, value, length=None):
    return element(eid, value.to_bytes(length or max(1, (value.bit_length() + 7) // 8), "big"))


def text(eid, value):
    return element(eid, value.encode())


def fp64(eid, value):
    return element(eid, struct.pack(">d", value))


def lace_xiph(sizes):
    head = bytes([len(sizes) - 1])
    for size in sizes[:-1]:
        head += b"\xff" * (size // 255) + bytes([size % 255])
    return head


def lace_ebml(sizes):
    head = bytes([len(sizes) - 1]) + vint(sizes[0])
    for i in range(1, len(sizes) - 1):
        delta = sizes[i] - sizes[i - 1]
        length = 1
        while not -(1 << (7 * length - 1)) + 1 <= delta <= (1 << (7 * length - 1)) - 1:
            length += 1
        head += ((1 << (7 * length)) | (delta + (1 << (7 * length - 1)) - 1)).to_bytes(length, "big")
    return head


def block(track, relative, flags, payload):
    return vint(track) + struct.pack(">hB", relative, flags) + payload


def synth_mkv(name, lacing=None, group=False, cues=True, tail_tags=False, encoded=False, unknown=False, projection=None):
    rng = random.Random(zlib.crc32(name.encode()))
    noise = lambda n: bytes(rng.getrandbits(8) for _ in range(n))
    video = uint(b"\xD7", 1) + uint(b"\x83", 1) + text(b"\x86", "V_MPEG4/ISO/AVC") + element(b"\x63\xA2", b"\x01\x64\x00\x1f\xff\xe1\x00\x00") + uint(b"\x23\xE3\x83", 40000000)
    picture = uint(b"\xB0", 160) + uint(b"\xBA", 120)
    if projection is not None:
        picture += element(b"\x76\x70", uint(b"\x76\x71", 0) + fp64(b"\x76\x75", projection))
    video += element(b"\xE0", picture)
    if encoded:
        video += element(b"\x6D\x80", element(b"\x62\x40", uint(b"\x50\x31", 0)))
    audio = uint(b"\xD7", 2) + uint(b"\x83", 2) + text(b"\x86", "A_AAC") + element(b"\x63\xA2", b"\x12\x10") + element(b"\xE1", fp64(b"\xB5", 44100.0) + uint(b"\x9F", 2))
    tracks = element(b"\x16\x54\xAE\x6B", element(b"\xAE", video) + element(b"\xAE", audio))
    info = element(b"\x15\x49\xA9\x66", uint(b"\x2A\xD7\xB1", 1000000) + text(b"\x7B\xA9", "Synth title") + element(b"\x44\x89", struct.pack(">f", 4000.0)))

    clusters = b""
    points = []
    head_room = 200
    base = None
    for c in range(8):
        time = c * 500
        body = uint(b"\xE7", time)
        for f in range(12):
            key = f == 0
            relative = f * 40
            payload = noise(rng.randrange(300, 1500))
            if group:
                inner = element(b"\xA1", block(1, relative, 0, payload))
                if not key:
                    inner += uint(b"\xFB", 40)
                body += element(b"\xA0", inner)
            else:
                body += element(b"\xA3", block(1, relative, 0x80 if key else 0, payload))
            if f % 3 == 0:
                sizes = [rng.randrange(40, 700) for _ in range(5)]
                blob = noise(sum(sizes))
                if lacing == "xiph":
                    body += element(b"\xA3", block(2, relative, 0x80 | 0x02, lace_xiph(sizes) + blob))
                elif lacing == "ebml":
                    body += element(b"\xA3", block(2, relative, 0x80 | 0x06, lace_ebml(sizes) + blob))
                elif lacing == "fixed":
                    body += element(b"\xA3", block(2, relative, 0x80 | 0x04, bytes([4]) + noise(500)))
                elif lacing == "broken":
                    body += element(b"\xA3", block(2, relative, 0x80 | 0x02, lace_xiph([60000, 3, 4, 5, 6]) + blob))
                else:
                    body += element(b"\xA3", block(2, relative, 0x80, blob))
        points.append((time, len(clusters)))
        if unknown:
            clusters += b"\x1F\x43\xB6\x75\x01\xFF\xFF\xFF\xFF\xFF\xFF\xFF" + body
        else:
            clusters += element(b"\x1F\x43\xB6\x75", body)

    tags = element(b"\x12\x54\xC3\x67", element(b"\x73\x73",
        element(b"\x67\xC8", text(b"\x45\xA3", "ALBUM") + text(b"\x44\x87", "Tail album")
                + element(b"\x67\xC8", text(b"\x45\xA3", "PART_NUMBER") + text(b"\x44\x87", "6")))
        + element(b"\x67\xC8", text(b"\x45\xA3", "DATE_RELEASED") + text(b"\x44\x87", "2012"))))
    attachments = element(b"\x19\x41\xA4\x69", element(b"\x61\xA7", text(b"\x46\x6E", "Cover.jpg") + text(b"\x46\x60", "image/jpeg") + element(b"\x46\x5C", COVER)))

    def seek_entry(eid, position):
        return element(b"\x4D\xBB", element(b"\x53\xAB", eid) + uint(b"\x53\xAC", position, 8))

    front = info + tracks
    seek_size = len(element(b"\x11\x4D\x9B\x74", seek_entry(b"\x1C\x53\xBB\x6B", 0) * 3))
    data_at = seek_size + len(front)
    end_at = data_at + len(clusters)
    cue_body = b""
    for time, offset in points:
        position = element(b"\xB7", uint(b"\xF7", 1) + uint(b"\xF1", data_at + offset, 4))
        cue_body += element(b"\xBB", uint(b"\xB3", time) + position)
    cue_blob = element(b"\x1C\x53\xBB\x6B", cue_body)
    trailer = cue_blob if cues else b""
    entries = seek_entry(b"\x1C\x53\xBB\x6B", end_at) if cues else seek_entry(b"\xEC\x00", 0)
    if tail_tags:
        entries += seek_entry(b"\x12\x54\xC3\x67", end_at + len(trailer))
        entries += seek_entry(b"\x19\x41\xA4\x69", end_at + len(trailer) + len(tags))
        trailer += tags + attachments
    else:
        entries += seek_entry(b"\xEC\x00", 0) * 2
    segment = element(b"\x11\x4D\x9B\x74", entries) + front + clusters + trailer
    ebml = element(b"\x1A\x45\xDF\xA3", text(b"\x42\x82", "matroska"))
    open(path(name), "wb").write(ebml + b"\x18\x53\x80\x67" + vint(len(segment), 8) + segment)


synth_mkv("laced_xiph.mkv", lacing="xiph")
synth_mkv("laced_ebml.mkv", lacing="ebml", cues=False)
synth_mkv("laced_fixed.mkv", lacing="fixed", tail_tags=True)
synth_mkv("laced_broken.mkv", lacing="broken")
synth_mkv("groups.mkv", group=True)
synth_mkv("groups_laced.mkv", group=True, lacing="ebml")
synth_mkv("tail_tags.mkv", tail_tags=True)
synth_mkv("encoded.mkv", encoded=True)
synth_mkv("unknown_clusters.mkv", unknown=True, lacing="xiph")
synth_mkv("roll90.mkv", projection=-90.0)
synth_mkv("roll33.mkv", projection=33.0)

# MP4
ffmpeg("h264_aac.mp4", *VIDEO, *AUDIO, *X264_B, "-c:a", "aac", "-b:a", "96k", *TAGS)
ffmpeg("faststart.mp4", *VIDEO, *AUDIO, *X264, "-c:a", "aac", "-movflags", "+faststart", *TAGS)
ffmpeg("mjpeg.mp4", *VIDEO, *AUDIO, *MJPEG, "-c:a", "aac")
ffmpeg("mjpeg.mov", *VIDEO, *AUDIO, *MJPEG, "-c:a", "pcm_s16le")
ffmpeg("mpeg2.mp4", *VIDEO, *AUDIO, *MPEG2, "-c:a", "libmp3lame")
ffmpeg("mpeg2.mov", *VIDEO, *MPEG2)
ffmpeg("h264_opus.mp4", *VIDEO, *AUDIO, *X264, "-ar", "48000", "-c:a", "libopus")
ffmpeg("h264_flac.mp4", *VIDEO, *AUDIO, *X264, "-c:a", "flac", "-strict", "-2")
ffmpeg("h264_alac.mp4", *VIDEO, *AUDIO, *X264, "-c:a", "alac")
ffmpeg("h264_mp3.mp4", *VIDEO, *AUDIO, *X264, "-c:a", "libmp3lame")
ffmpeg("h264_sowt.mov", *VIDEO, *AUDIO, *X264, "-c:a", "pcm_s16le")
ffmpeg("rotated.mp4", "-display_rotation", "90", "-i", path("h264_aac.mp4"), "-c", "copy")
ffmpeg("rotated_neg.mp4", "-display_rotation", "270", "-i", path("h264_aac.mp4"), "-c", "copy")
ffmpeg("rotated180.mp4", "-display_rotation", "180", "-i", path("h264_aac.mp4"), "-c", "copy")
ffmpeg("rotated30.mp4", "-display_rotation", "30", "-i", path("h264_aac.mp4"), "-c", "copy")
ffmpeg("aac_only.m4a", *AUDIO, "-c:a", "aac", *TAGS)
ffmpeg("alac_only.m4a", *AUDIO, "-c:a", "alac")
ffmpeg("cover.m4a", *AUDIO, "-i", path("cover.jpg"), "-map", "0:a", "-map", "1:v", "-c:a", "aac", "-c:v", "copy", "-disposition:v", "attached_pic", *TAGS)
ffmpeg("fragmented.mp4", *VIDEO, *X264, "-movflags", "frag_keyframe+empty_moov")
ffmpeg("delayed.mp4", *VIDEO, "-itsoffset", "0.5", *AUDIO, *X264, "-c:a", "aac")
ffmpeg("video_only.mp4", *VIDEO, *X264)
ffmpeg("hevc.mp4", *VIDEO, "-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=none")
ffmpeg("audio_unsupported.mp4", *VIDEO, *AUDIO, *X264, "-c:a", "ac3")
ffmpeg("slideshow.mp4", *VIDEO, *MJPEG, "-vf", "select='not(mod(n\\,10))'", "-fps_mode", "vfr")
ffmpeg("large.mp4", *LARGE, "-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v", "2")
ffmpeg("noninterleaved.mov", *VIDEO, *AUDIO, *X264, "-c:a", "pcm_s16le", "-movflags", "+faststart")



# Sample tables ffmpeg only writes for files too large to keep around.
def boxes(data):
    at = 0
    while at < len(data):
        size, kind = struct.unpack_from(">I4s", data, at)
        yield kind, data[at + 8 : at + size]
        at += size


def box(kind, body):
    return struct.pack(">I4s", len(body) + 8, kind) + body


def wide_tables(body):
    out = b""
    for kind, inner in boxes(body):
        if kind in (b"trak", b"mdia", b"minf", b"stbl"):
            inner = wide_tables(inner)
        elif kind == b"stco":
            count = struct.unpack_from(">I", inner, 4)[0]
            offsets = struct.unpack_from(f">{count}I", inner, 8)
            kind, inner = b"co64", inner[:8] + struct.pack(f">{count}Q", *offsets)
        elif kind == b"stsz" and struct.unpack_from(">I", inner, 4)[0] == 0:
            count = struct.unpack_from(">I", inner, 8)[0]
            sizes = struct.unpack_from(f">{count}I", inner, 12)
            if max(sizes) < 65536:
                kind, inner = b"stz2", inner[:4] + b"\0\0\0\x10" + struct.pack(">I", count) + struct.pack(f">{count}H", *sizes)
        out += box(kind, inner)
    return out


def rewrite_tables(src, dst):
    data = open(path(src), "rb").read()
    at = data.rfind(b"moov") - 4
    size = struct.unpack_from(">I", data, at)[0]
    assert at + size == len(data)
    open(path(dst), "wb").write(data[:at] + box(b"moov", wide_tables(data[at + 8 : at + size])))


rewrite_tables("h264_aac.mp4", "co64_stz2.mp4")

# Files cut short and files with bytes stomped on, kept to what the demuxers
# have to survive.
SOURCES = ["mjpeg_pcm.avi", "h264_mp3.avi", "s16.wav", "ima.wav", "xing.mp3", "adts.aac", "tagged.flac", "cover.flac",
           "plain.opus", "cover.opus", "h264_aac.mkv", "laced_xiph.mkv", "mjpeg_pcm.mkv", "h264_aac.mp4", "faststart.mp4", "cover.m4a"]
for name in SOURCES:
    if not os.path.exists(path(name)):
        continue
    data = open(path(name), "rb").read()
    stem, ext = os.path.splitext(name)
    for percent in (5, 40, 90):
        open(path(f"cut{percent}_{stem}{ext}"), "wb").write(data[: len(data) * percent // 100])
    for seed in range(3):
        rng = random.Random(zlib.crc32(name.encode()) + seed)
        mutated = bytearray(data)
        for _ in range(40):
            mutated[rng.randrange(len(mutated))] = rng.getrandbits(8)
        open(path(f"stomp{seed}_{stem}{ext}"), "wb").write(mutated)

for name in ("cover.jpg", "cover.png", "large_cover.jpg"):
    os.remove(path(name))
