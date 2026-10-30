/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Decodes a test/packetize.py stream with audio_framework and writes what the
 * crate's tests compare against: per packet i32 ok, u32 frames, u32 final
 * range, then the PCM; the first 64 packets again after a reset.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "audf_aac.h"
#include "audf_alac.h"
#include "audf_flac.h"
#include "audf_mp3.h"
#include "audf_opus.h"

static uint8_t *read_file(const char *path, size_t *size) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    *size = (size_t)ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *data = malloc(*size + 1);
    if (fread(data, 1, *size, f) != *size) exit(2);
    fclose(f);
    return data;
}

static uint32_t u32le(const uint8_t *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    size_t size;
    uint8_t *file = read_file(argv[1], &size);
    if (!file || memcmp(file, "PKT1", 4)) return 2;
    const uint8_t codec = file[4], p1 = file[5], p2 = file[6];
    const uint32_t cfg_len = u32le(file + 8);
    const uint8_t *cfg = file + 12;
    const uint8_t *cursor = cfg + cfg_len;
    const uint32_t count = u32le(cursor);
    cursor += 4;

    audf_decoder_t *dec = NULL;
    esp_err_t err = ESP_FAIL;
    switch (codec) {
    case 0: {
        const audf_aac_config_t c = { .asc = cfg, .asc_len = cfg_len, .adts = p1, .he = (audf_aac_he_t)p2 };
        err = audf_aac_decoder_create(&c, &dec);
        break;
    }
    case 1:
        err = audf_mp3_decoder_create(&(audf_mp3_config_t){ .channels = p1 }, &dec);
        break;
    case 2:
        err = audf_opus_decoder_create(&(audf_opus_config_t){ .head = cfg, .head_len = cfg_len }, &dec);
        break;
    case 3:
        err = audf_flac_decoder_create(&(audf_flac_config_t){ .streaminfo = cfg, .streaminfo_len = cfg_len }, &dec);
        break;
    default:
        err = audf_alac_decoder_create(&(audf_alac_config_t){ .cookie = cfg, .cookie_len = cfg_len }, &dec);
        break;
    }
    FILE *out = fopen(argv[2], "wb");
    const int32_t created = err == ESP_OK;
    fwrite(&created, 4, 1, out);
    if (err != ESP_OK) {
        fclose(out);
        return 0;
    }
    const size_t bytes = audf_decoder_fmt(dec) == AUDF_FMT_S16 ? 2 : 4;
    const size_t channels = audf_decoder_channels(dec);
    void *pcm = malloc(audf_decoder_max_frames(dec) * channels * bytes);
    for (int pass = 0; pass < 2; pass++) {
        const uint8_t *p = cursor;
        const uint32_t n = pass ? (count < 64 ? count : 64) : count;
        if (pass) audf_decoder_reset(dec);
        for (uint32_t i = 0; i < n; i++) {
            const uint32_t len = u32le(p);
            p += 4;
            size_t frames = 0;
            err = audf_decoder_decode(dec, p, len, pcm, &frames);
            p += len;
            const int32_t ok = err == ESP_OK;
            const uint32_t f = ok ? (uint32_t)frames : 0;
            const uint32_t range = codec == 2 ? audf_opus_decoder_final_range(dec) : 0;
            fwrite(&ok, 4, 1, out);
            fwrite(&f, 4, 1, out);
            fwrite(&range, 4, 1, out);
            fwrite(pcm, bytes, f * channels, out);
        }
    }
    fclose(out);
    audf_decoder_destroy(dec);
    free(pcm);
    free(file);
    return 0;
}
