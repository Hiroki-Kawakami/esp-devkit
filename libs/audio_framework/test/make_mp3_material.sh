#!/bin/sh
# Synthetic MP3 and MP2 material for test/mp3_dec_test.c, encoded by ffmpeg
# (libmp3lame and its own MP2 encoder), lame and twolame, plus mpg123's decode
# of each file (<name>.ref.raw, s16le) to compare against. ffmpeg's s16 output
# is half a step low and its MP2 decoder a little off, so it is not the
# reference. Everything comes from a temporary nix-shell, so nothing is added
# to the flake:
#   esp-devkit/libs/audio_framework/test/make_mp3_material.sh <out dir>
set -e

OUT=${1:?usage: make_mp3_material.sh <out dir>}
mkdir -p "$OUT"
OUT=$(CDPATH= cd -- "$OUT" && pwd)

nix-shell -p ffmpeg lame twolame mpg123 --run "sh -s '$OUT'" <<'EOF'
set -e
OUT=$1
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT

# Tones, a chirp and noise bursts (which bring short blocks), different per
# channel, with a loud stretch that clips the scale factors high.
src() {
    rate=$1; secs=$2
    ffmpeg -v error -y -f lavfi -i "aevalsrc=exprs=\
(0.25*sin(2*PI*(220+110*floor(mod(t\,4)))*t)*(0.6+0.4*sin(2*PI*0.5*t))\
+0.12*sin(2*PI*(500*t+1500*t*t/$secs))\
+0.3*(random(0)-0.5)*lt(mod(t\,0.37)\,0.03))*(1+1.5*between(t\,2\,2.5))\
|0.25*sin(2*PI*(330+110*floor(mod(t\,3)))*t)\
+0.12*sin(2*PI*($rate/6-300*t)*t)\
+0.10*(random(1)-0.5)*(0.5+0.5*sin(2*PI*0.25*t)):s=$rate:d=$secs" \
        -c:a pcm_s16le "$T/$rate.wav"
}

for r in 48000 44100 32000 24000 22050 16000 12000 11025 8000; do src $r 6; done

ref() {
    mpg123 -q -e s16 -s "$OUT/$1" > "$OUT/${1%.*}.ref.raw"
}

ff() {
    name=$1; rate=$2; shift 2
    ffmpeg -v error -y -i "$T/$rate.wav" -c:a libmp3lame -write_xing 0 "$@" "$OUT/$name.mp3"
    ref "$name.mp3"
}

ff l3_44_128 44100 -b:a 128k
ff l3_44_320 44100 -b:a 320k
ff l3_44_vbr 44100 -q:a 2
ff l3_44_stereo 44100 -b:a 192k -joint_stereo 0
ff l3_44_nores 44100 -b:a 128k -reservoir 0
ff l3_44_mono 44100 -b:a 64k -ac 1
ff l3_48_160 48000 -b:a 160k
ff l3_32_96 32000 -b:a 96k
ff l3_24_64 24000 -b:a 64k
ff l3_22_48 22050 -b:a 48k
ff l3_16_32 16000 -b:a 32k -ac 1
ff l3_12_32 12000 -b:a 32k
ff l3_11_24 11025 -b:a 24k
ff l3_8_8 8000 -b:a 8k -ac 1
ff l3_8_16 8000 -b:a 16k

lame --quiet -t -m f -b 128 "$T/44100.wav" "$OUT/l3_44_forced_ms.mp3"
ref l3_44_forced_ms.mp3
lame --quiet -t -V 0 --resample 48 "$T/48000.wav" "$OUT/l3_48_v0.mp3"
ref l3_48_v0.mp3

mp2() {
    name=$1; rate=$2; shift 2
    ffmpeg -v error -y -i "$T/$rate.wav" -c:a mp2 "$@" "$OUT/$name.mp2"
    ref "$name.mp2"
}

mp2 l2_48_192 48000 -b:a 192k
mp2 l2_44_384 44100 -b:a 384k
mp2 l2_44_256 44100 -b:a 256k
mp2 l2_32_192 32000 -b:a 192k
mp2 l2_44_64 44100 -b:a 64k
mp2 l2_32_64 32000 -b:a 64k
mp2 l2_48_mono 48000 -b:a 96k -ac 1
mp2 l2_24_96 24000 -b:a 96k
mp2 l2_22_64 22050 -b:a 64k
mp2 l2_16_32 16000 -b:a 32k -ac 1

twolame --quiet -m j -b 128 "$T/44100.wav" "$OUT/l2_44_joint.mp2"
ref l2_44_joint.mp2
twolame --quiet -m j -b 96 "$T/48000.wav" "$OUT/l2_48_joint.mp2"
ref l2_48_joint.mp2
twolame --quiet -m d -b 192 "$T/32000.wav" "$OUT/l2_32_dual.mp2"
ref l2_32_dual.mp2
twolame --quiet -p -b 160 "$T/44100.wav" "$OUT/l2_44_crc.mp2"
ref l2_44_crc.mp2
ls -l "$OUT"
EOF
