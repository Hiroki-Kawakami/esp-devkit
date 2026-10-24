/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "medf_buffer.h"
#include "medf_tags.h"
#include "medf_riff.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    medf_riff_audio_codec_t codec;
    uint32_t sample_rate;
    uint32_t bitrate_bps;
    uint8_t channels;
    uint8_t bits_per_sample;
    uint16_t block_align;
    const uint8_t *codec_private;
    uint32_t codec_private_size;
    int64_t duration_us;
    medf_tags_t tags;
} medf_wav_info_t;

typedef struct {
    int64_t pts_us;
    const uint8_t *data;
    uint32_t size;
    uint32_t ref;
} medf_wav_packet_t;

typedef struct medf_wav_demux medf_wav_demux_t;

medf_wav_demux_t *medf_wav_demux_open(const char *path, const medf_arena_t *arena, bool want_cover,
                                      const char **error);
void medf_wav_demux_close(medf_wav_demux_t *demux);

const medf_wav_info_t *medf_wav_demux_info(const medf_wav_demux_t *demux);
medf_buffer_t *medf_wav_demux_buffer(medf_wav_demux_t *demux);

bool medf_wav_demux_read(medf_wav_demux_t *demux, medf_wav_packet_t *packet);
bool medf_wav_demux_seek(medf_wav_demux_t *demux, int64_t pts_us, int64_t *landed_us);

#ifdef __cplusplus
}
#endif
