#!/bin/sh
# Synthetic FLAC material for test/flac_dec_test.c, encoded with the reference
# encoder and with ffmpeg's. Both come from a temporary nix-shell, so nothing
# is added to the flake:
#   esp-devkit/libs/audio_framework/test/make_flac_material.sh <out dir>
set -e

OUT=${1:?usage: make_flac_material.sh <out dir>}
mkdir -p "$OUT"
OUT=$(CDPATH= cd -- "$OUT" && pwd)

nix-shell -p ffmpeg flac --run "sh -s '$OUT'" <<'EOF'
set -e
OUT=$1
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT

# Tones, a chirp, noise bursts and a stretch of digital silence, so every
# subframe type turns up; the right channel differs so each stereo mode wins
# somewhere.
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

ref() {
    name=$1; wav=$2; shift 2
    flac -s -f "$@" -o "$OUT/$name.flac" "$wav"
}

src 44100 10 pcm_s16le "$T/s16_44.wav"
src 48000 10 pcm_s24le "$T/s24_48.wav"
src 96000 5 pcm_s24le "$T/s24_96.wav"
src 22050 10 pcm_u8 "$T/u8_22.wav"
ffmpeg -v error -y -i "$T/s16_44.wav" -fflags +bitexact -ac 1 "$T/s16_44_mono.wav"
# 16-bit samples in a 24-bit container: every subframe has 8 wasted bits.
ffmpeg -v error -y -i "$T/s16_44.wav" -fflags +bitexact -c:a pcm_s24le "$T/s24_wasted.wav"

ref s16_44_l0 "$T/s16_44.wav" -0
ref s16_44_l5 "$T/s16_44.wav" -5
ref s16_44_l8 "$T/s16_44.wav" -8
ref s16_44_lax32 "$T/s16_44.wav" --lax -8 -l 32 -r 15 -b 8192
ref s16_44_b192 "$T/s16_44.wav" -5 -b 192
ref s16_44_b16384 "$T/s16_44.wav" --lax -5 -b 16384
ref s16_44_mono "$T/s16_44_mono.wav" -5
ref s24_48_l8 "$T/s24_48.wav" -8
ref s24_96_l5 "$T/s24_96.wav" -5
ref s24_wasted "$T/s24_wasted.wav" -5
ref u8_22_l5 "$T/u8_22.wav" -5

ff() {
    name=$1; wav=$2; shift 2
    ffmpeg -v error -y -i "$wav" -c:a flac "$@" "$OUT/$name.flac"
}

ff ff_s16_l12 "$T/s16_44.wav" -compression_level 12
ff ff_s16_indep "$T/s16_44.wav" -ch_mode indep
ff ff_s16_ls "$T/s16_44.wav" -ch_mode left_side
ff ff_s16_rs "$T/s16_44.wav" -ch_mode right_side
ff ff_s16_ms "$T/s16_44.wav" -ch_mode mid_side
ff ff_s24_l12 "$T/s24_48.wav" -compression_level 12 -exact_rice_parameters 1
ls -l "$OUT"
EOF
