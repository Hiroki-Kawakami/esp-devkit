#!/bin/sh
# Synthetic Ogg Opus material for test/opus_dec_test.c, encoded by ffmpeg with
# libopus (and one file by opusenc, for tags and a cover), plus libopus's own
# decode of each file (<name>.ref.raw, 48 kHz s16le) to compare against. Both
# come from a temporary nix-shell, so nothing is added to the flake:
#   esp-devkit/libs/audio_framework/test/make_opus_material.sh <out dir>
set -e

OUT=${1:?usage: make_opus_material.sh <out dir>}
mkdir -p "$OUT"
OUT=$(CDPATH= cd -- "$OUT" && pwd)

nix-shell -p ffmpeg opus-tools --run "sh -s '$OUT'" <<'EOF'
set -e
OUT=$1
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT

# Music-like: tones, a chirp and noise bursts, different per channel.
ffmpeg -v error -y -f lavfi -i "aevalsrc=exprs=\
0.25*sin(2*PI*(220+110*floor(mod(t\,4)))*t)*(0.6+0.4*sin(2*PI*0.5*t))\
+0.12*sin(2*PI*(500*t+1500*t*t/10))\
+0.15*(random(0)-0.5)*lt(mod(t\,0.37)\,0.04)\
|0.25*sin(2*PI*(330+110*floor(mod(t\,3)))*t)\
+0.12*sin(2*PI*(9000-500*t)*t)\
+0.10*(random(1)-0.5)*(0.5+0.5*sin(2*PI*0.25*t)):s=48000:d=10" \
    -c:a pcm_s16le "$T/music.wav"

# Speech-like: a gliding harmonic buzz through a moving resonance, broken by
# noise and silence, so the encoder picks SILK and Hybrid.
ffmpeg -v error -y -f lavfi -i "aevalsrc=exprs=\
(0.3*sin(2*PI*(120+40*sin(2*PI*0.7*t))*t)*(1+0.8*sin(2*PI*(600+300*sin(2*PI*0.3*t))*t))\
*lt(mod(t\,0.8)\,0.5)\
+0.08*(random(0)-0.5)*gt(mod(t\,0.8)\,0.55)*lt(mod(t\,0.8)\,0.7))\
|(0.3*sin(2*PI*(150+50*sin(2*PI*0.5*t))*t)*(1+0.8*sin(2*PI*(800+200*sin(2*PI*0.2*t))*t))\
*lt(mod(t+0.3\,0.8)\,0.5)\
+0.08*(random(1)-0.5)*gt(mod(t+0.3\,0.8)\,0.55)):s=48000:d=10" \
    -c:a pcm_s16le "$T/speech.wav"

enc() {
    name=$1; src=$2; shift 2
    ffmpeg -v error -y -i "$T/$src.wav" -c:a libopus "$@" "$OUT/$name.opus"
    ffmpeg -v error -y -c:a libopus -i "$OUT/$name.opus" -ar 48000 -f s16le "$OUT/$name.ref.raw"
}

enc music_128k music -b:a 128k
enc music_64k music -b:a 64k
enc music_32k music -b:a 32k
enc music_cbr96 music -b:a 96k -vbr off
enc music_2_5ms music -b:a 128k -frame_duration 2.5
enc music_5ms music -b:a 96k -frame_duration 5
enc music_10ms music -b:a 96k -frame_duration 10
enc music_40ms music -b:a 64k -frame_duration 40
enc music_60ms music -b:a 64k -frame_duration 60
enc music_mono music -b:a 48k -ac 1
enc speech_8k speech -b:a 8k -ac 1 -application voip
enc speech_12k speech -b:a 12k -ac 1 -application voip
enc speech_16k_stereo speech -b:a 16k -application voip
enc speech_24k speech -b:a 24k -ac 1 -application voip
enc speech_32k_stereo speech -b:a 32k -application voip
enc speech_nb speech -b:a 10k -ac 1 -application voip -cutoff 4000
enc speech_60ms speech -b:a 12k -ac 1 -application voip -frame_duration 60
enc speech_fec speech -b:a 24k -ac 1 -application voip -fec 1 -packet_loss 10
# Pages twenty seconds long fill all 255 lacing values, so packets run on
# into the next page.
enc music_spanning music -b:a 256k -page_duration 20000000

ffmpeg -v error -y -f lavfi -i testsrc=size=320x320:rate=1 -frames:v 1 "$T/cover.jpg"
opusenc --quiet --bitrate 96 --title "Synthetic Title" --artist "Test Artist" --album "Test Album" \
    --comment TRACKNUMBER=3 --comment TRACKTOTAL=12 --picture "$T/cover.jpg" \
    "$T/music.wav" "$OUT/tagged.opus"
ffmpeg -v error -y -c:a libopus -i "$OUT/tagged.opus" -ar 48000 -f s16le "$OUT/tagged.ref.raw"
ls -l "$OUT"
EOF
