#!/bin/sh
# Synthetic ALAC material for test/alac_dec_test.c: each <name>.m4a comes with
# <name>.raw, the source samples as s16le or s24le. ffmpeg comes from a
# temporary nix-shell; Apple's encoder is macOS's afconvert, skipped elsewhere.
#   esp-devkit/libs/audio_framework/test/make_alac_material.sh <out dir>
set -e

OUT=${1:?usage: make_alac_material.sh <out dir>}
mkdir -p "$OUT"
OUT=$(CDPATH= cd -- "$OUT" && pwd)

nix-shell -p ffmpeg --run "sh -s '$OUT'" <<'EOF'
set -e
OUT=$1
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT

# Tones, a chirp, noise bursts and a stretch of digital silence, so runs of
# zeros and both stereo modes turn up.
src() {
    rate=$1; secs=$2; fmt=$3; out=$4
    ffmpeg -v error -y -f lavfi -i "aevalsrc=exprs=\
(0.25*sin(2*PI*(220+110*floor(mod(t\,4)))*t)*(0.6+0.4*sin(2*PI*0.5*t))\
+0.12*sin(2*PI*(500*t+1500*t*t/$secs))\
+0.15*(random(0)-0.5)*lt(mod(t\,0.37)\,0.04))*gt(mod(t\,5)\,0.5)\
|(0.25*sin(2*PI*(330+110*floor(mod(t\,3)))*t)\
+0.12*sin(2*PI*(9000-500*t)*t)\
+0.10*(random(1)-0.5)*(0.5+0.5*sin(2*PI*0.25*t)))*gt(mod(t\,5)\,0.5):s=$rate:d=$secs" \
        -fflags +bitexact -c:a "$fmt" "$out"
}

src 44100 10 pcm_s16le "$T/s16_44.wav"
src 48000 10 pcm_s24le "$T/s24_48.wav"
src 96000 5 pcm_s24le "$T/s24_96.wav"
ffmpeg -v error -y -i "$T/s16_44.wav" -fflags +bitexact -ac 1 "$T/s16_44_mono.wav"
# 20-bit samples in a 24-bit container, for Apple's 20-bit mode.
ffmpeg -v error -y -i "$T/s24_48.wav" -fflags +bitexact \
    -af "aeval=floor(val(0)*524288)/524288|floor(val(1)*524288)/524288" -c:a pcm_s24le "$T/s20_48.wav"

for w in "$T"/*.wav; do
    n=$(basename "$w" .wav)
    case $n in s24*|s20*) f=s24le ;; *) f=s16le ;; esac
    ffmpeg -v error -y -i "$w" -f "$f" "$T/$n.raw"
done

ff() {
    name=$1; base=$2; shift 2
    ffmpeg -v error -y -i "$T/$base.wav" -c:a alac "$@" "$OUT/$name.m4a"
    cp "$T/$base.raw" "$OUT/$name.raw"
}

ff ff_s16_44 s16_44
ff ff_s16_44_mono s16_44_mono
ff ff_s16_44_min s16_44 -compression_level 0
ff ff_s16_44_max s16_44 -compression_level 2 -min_prediction_order 1 -max_prediction_order 30
ff ff_s24_48 s24_48
ff ff_s24_96 s24_96
cp "$T"/*.wav "$T"/*.raw "$OUT/"
EOF

if command -v afconvert >/dev/null; then
    for n in s16_44 s16_44_mono s24_48 s24_96; do
        afconvert -f m4af -d alac "$OUT/$n.wav" "$OUT/af_$n.m4a"
        cp "$OUT/$n.raw" "$OUT/af_$n.raw"
    done
    afconvert -f m4af -d alac/2 "$OUT/s20_48.wav" "$OUT/af_s20_48.m4a"
    cp "$OUT/s20_48.raw" "$OUT/af_s20_48.raw"
    # 352-frame packets instead of 4096.
    afconvert -f m4af -d 'alac#352' "$OUT/s16_44.wav" "$OUT/af_s16_44_352.m4a"
    cp "$OUT/s16_44.raw" "$OUT/af_s16_44_352.raw"
fi
rm -f "$OUT"/*.wav
for r in "$OUT"/*.raw; do
    [ -e "${r%.raw}.m4a" ] || rm -f "$r"
done
ls -l "$OUT"
