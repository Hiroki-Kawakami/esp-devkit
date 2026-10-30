#!/usr/bin/env python3
# Cuts about three seconds off each stream the bench decodes, out of the
# directory libs/devkit_audio/test/packetize.py wrote.
#   make_clips.py <packets dir> <clips dir>
import pathlib
import struct
import sys

CLIPS = {
    "mp3_l3_44_128mp3_2": 115,
    "mp3_l3_44_320mp3_2": 115,
    "mp3_l2_44_256mp2_2": 115,
    "aac_adts_lc_44_2_128_he0": 130,
    "aac_adts_he_48_2_64_he1": 70,
    "aac_adts_ps_48_2_32_he2": 70,
    "opus_music_128k": 150,
    "opus_speech_16k_stereo": 150,
    "opus_speech_8k": 150,
    "flac_s16_44_l5": 35,
    "flac_s24_96_l5": 75,
    "alac_ff_s16_44": 35,
}


def main(src, dst):
    src, dst = pathlib.Path(src), pathlib.Path(dst)
    dst.mkdir(parents=True, exist_ok=True)
    for name, limit in CLIPS.items():
        data = (src / f"{name}.pkt").read_bytes()
        cfg_len = struct.unpack("<I", data[8:12])[0]
        at = 12 + cfg_len
        count = struct.unpack("<I", data[at:at + 4])[0]
        head = data[:at]
        at += 4
        start = at
        keep = min(count, limit)
        for _ in range(keep):
            at += 4 + struct.unpack("<I", data[at:at + 4])[0]
        (dst / f"{name}.pkt").write_bytes(head + struct.pack("<I", keep) + data[start:at])


main(sys.argv[1], sys.argv[2])
