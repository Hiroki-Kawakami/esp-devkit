/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "audf_mp3.h"

static uint8_t *read_file(const char *path, size_t *size) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    const long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *buf = malloc((size_t)n + 1);
    if (buf && fread(buf, 1, (size_t)n, f) != (size_t)n) {
        free(buf);
        buf = NULL;
    }
    fclose(f);
    *size = (size_t)n;
    return buf;
}

static size_t skip_id3(const uint8_t *buf, size_t size) {
    if (size < 10 || memcmp(buf, "ID3", 3)) return 0;
    const size_t len =
        (size_t)(buf[6] & 0x7f) << 21 | (buf[7] & 0x7f) << 14 | (buf[8] & 0x7f) << 7 | (buf[9] & 0x7f);
    return 10 + len + (buf[5] & 0x10 ? 10 : 0);
}

static bool info_frame(const uint8_t *frame, size_t len) {
    for (size_t i = 4; i + 4 <= len && i < 40; i++) {
        if (!memcmp(frame + i, "Xing", 4) || !memcmp(frame + i, "Info", 4) || !memcmp(frame + i, "VBRI", 4)) {
            return true;
        }
    }
    return false;
}

int main(int argc, char **argv) {
    const char *in = NULL, *out = NULL, *ref = NULL;
    int loops = 1, channels = 0;
    bool native = false;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--loops") && i + 1 < argc) {
            loops = atoi(argv[++i]);
        } else if (!strcmp(argv[i], "--channels") && i + 1 < argc) {
            channels = atoi(argv[++i]);
        } else if (!strcmp(argv[i], "--native")) {
            native = true;
        } else if (!strcmp(argv[i], "--ref") && i + 1 < argc) {
            ref = argv[++i];
        } else if (!in) {
            in = argv[i];
        } else {
            out = argv[i];
        }
    }
    if (!in) {
        fprintf(stderr, "usage: mp3_dec_test [--loops n] [--channels n] [--native] [--ref ref.raw]"
                        " in.mp3 [out.raw]\n");
        return 2;
    }
    size_t size;
    uint8_t *buf = read_file(in, &size);
    if (!buf) {
        fprintf(stderr, "cannot read %s\n", in);
        return 2;
    }

    size_t pos = skip_id3(buf, size);
    audf_mp3_header_t first;
    while (pos + AUDF_MP3_HEADER_BYTES <= size && !audf_mp3_parse_header(buf + pos, &first)) pos++;
    if (pos + AUDF_MP3_HEADER_BYTES > size) {
        fprintf(stderr, "no Layer II or III frame in %s\n", in);
        return 2;
    }
    if (pos + first.bytes <= size && info_frame(buf + pos, first.bytes)) pos += first.bytes;
    const size_t start = pos;

    const audf_mp3_config_t config = { .channels = (uint8_t)(channels ? channels : first.channels) };
    audf_decoder_t *dec;
    esp_err_t err = audf_mp3_decoder_create(&config, &dec);
    if (err != ESP_OK) {
        fprintf(stderr, "create: %d\n", err);
        return 2;
    }
    const int ch = audf_decoder_channels(dec);
    int16_t *pcm = malloc(audf_decoder_max_frames(dec) * ch * sizeof(int16_t));
    size_t ref_size = 0;
    uint8_t *ref_buf = ref ? read_file(ref, &ref_size) : NULL;
    if (ref && !ref_buf) {
        fprintf(stderr, "cannot read %s\n", ref);
        return 2;
    }
    const int16_t *ref_pcm = (const int16_t *)ref_buf;
    const size_t ref_samples = ref_size / sizeof(int16_t);
    double signal = 0, noise = 0;
    int max_diff = 0;
    size_t compared = 0, frames_total = 0, samples_total = 0, packets = 0, errors = 0;
    FILE *f = out ? fopen(out, "wb") : NULL;

    const clock_t t0 = clock();
    for (int loop = 0; loop < loops; loop++) {
        audf_decoder_reset(dec);
        size_t written = 0, samples = 0;
        for (pos = start; pos + AUDF_MP3_HEADER_BYTES <= size;) {
            audf_mp3_header_t h;
            if (!audf_mp3_parse_header(buf + pos, &h)) {
                pos++;
                continue;
            }
            if (pos + h.bytes > size) break;
            size_t frames = 0;
            err = audf_decoder_decode(dec, buf + pos, h.bytes, pcm, &frames);
            pos += h.bytes;
            if (loop) continue;
            packets++;
            if (err != ESP_OK) {
                if (!errors++) fprintf(stderr, "frame %zu: error %d\n", packets - 1, err);
                continue;
            }
            size_t n = frames * ch;
            if (native && h.channels < ch) {
                for (size_t k = 0; k < frames; k++) pcm[k] = pcm[k * ch];
                n = frames;
            }
            for (size_t k = 0; ref_pcm && k < n; k++) {
                const size_t at = samples + k;
                if (at >= ref_samples) break;
                const int d = pcm[k] - ref_pcm[at];
                signal += (double)ref_pcm[at] * ref_pcm[at];
                noise += (double)d * d;
                if (abs(d) > max_diff) max_diff = abs(d);
                compared++;
            }
            if (f) fwrite(pcm, sizeof(int16_t), n, f);
            written += frames;
            samples += n;
        }
        if (!loop) {
            frames_total = written;
            samples_total = samples;
        }
    }
    const double seconds = (double)(clock() - t0) / CLOCKS_PER_SEC;
    if (f) fclose(f);

    const uint32_t rate = audf_mp3_decoder_rate(dec);
    printf("Layer %s, %u Hz, %d ch, %zu frames, %zu packets, %zu errors, %.1fx realtime\n",
           first.layer == 2 ? "II" : "III", rate, ch, frames_total, packets, errors,
           seconds > 0 && rate ? loops * (double)frames_total / rate / seconds : 0);
    int status = errors ? 1 : 0;
    if (ref_pcm) {
        const double rms = compared ? sqrt(noise / compared) : 0;
        const char *accuracy = rms < 1 / sqrt(12) && max_diff <= 2 ? "full"
                               : rms < 16 / sqrt(12)                ? "limited"
                                                                    : "none";
        printf("against %s: SNR %.1f dB, rms %.3f, max difference %d, %s accuracy%s\n", ref,
               10 * log10((signal + 1) / (noise + 1)), rms, max_diff, accuracy,
               samples_total == ref_samples ? "" : ", lengths differ");
        if (strcmp(accuracy, "full")) status = 1;
    }
    audf_decoder_destroy(dec);
    free(pcm);
    free(ref_buf);
    free(buf);
    return status;
}
