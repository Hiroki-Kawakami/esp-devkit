#!/bin/sh
# Synthetic AAC material for test/aac_dec_test.c, encoded with fdk-aac's
# aac-enc (ADTS) and remuxed to MP4 by ffmpeg. Both come from a temporary
# nix-shell, so nothing is added to the flake:
#   esp-devkit/libs/audio_framework/test/make_aac_material.sh <out dir>
set -e

OUT=${1:?usage: make_aac_material.sh <out dir>}
mkdir -p "$OUT"
OUT=$(CDPATH= cd -- "$OUT" && pwd)

export NIXPKGS_ALLOW_UNFREE=1
nix-shell -p ffmpeg 'fdk_aac.override { exampleSupport = true; }' --run "sh -s '$OUT'" <<'EOF'
set -e
OUT=$1
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT

# Tones, a chirp and noise bursts: tonal content for the SBR sinusoid and
# envelope tools, transients for short windows and noise for PNS and noise
# floors. The right channel differs so stereo tools and PS have work to do.
src() {
    rate=$1; secs=$2; out=$3
    ffmpeg -v error -y -f lavfi -i "aevalsrc=exprs=\
0.25*sin(2*PI*(220+110*floor(mod(t\,4)))*t)*(0.6+0.4*sin(2*PI*0.5*t))\
+0.12*sin(2*PI*(500*t+1500*t*t/$secs))\
+0.10*sin(2*PI*3520*t)*lt(mod(t\,1)\,0.5)\
+0.15*(random(0)-0.5)*lt(mod(t\,0.37)\,0.04)\
|0.25*sin(2*PI*(330+110*floor(mod(t\,3)))*t)\
+0.12*sin(2*PI*(9000-500*t)*t)*gt(t\,0.2)\
+0.10*(random(1)-0.5)*(0.5+0.5*sin(2*PI*0.25*t)):s=$rate:d=$secs" \
        -c:a pcm_s16le "$out"
}

enc() {
    name=$1; rate=$2; ch=$3; aot=$4; br=$5; secs=$6
    src "$rate" "$secs" "$T/$name.stereo.wav"
    if [ "$ch" = 1 ]; then
        ffmpeg -v error -y -i "$T/$name.stereo.wav" -ac 1 "$T/$name.wav"
    else
        mv "$T/$name.stereo.wav" "$T/$name.wav"
    fi
    aac-enc -t "$aot" -r "$br" "$T/$name.wav" "$OUT/$name.aac" > /dev/null
    ffmpeg -v error -y -i "$OUT/$name.aac" -c copy "$OUT/$name.m4a"
}

enc lc_44_2_128   44100 2 2  128000 10
enc lc_48_2_96    48000 2 2  96000  10
enc lc_48_1_64    48000 1 2  64000  10
enc lc_22_2_48    22050 2 2  48000  10
enc lc_08_1_16    8000  1 2  16000  10
enc lc_96_2_256   96000 2 2  256000 5
enc he_48_2_64    48000 2 5  64000  10
enc he_44_2_48    44100 2 5  48000  10
enc he_32_1_24    32000 1 5  24000  10
enc he_22_2_32    22050 2 5  32000  10
enc ps_48_2_32    48000 2 29 32000  10
enc ps_44_2_24    44100 2 29 24000  10
ls -l "$OUT"
EOF
