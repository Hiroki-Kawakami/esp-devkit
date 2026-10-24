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

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    MEDF_ES_CODEC_NONE,
    MEDF_ES_CODEC_MP3,
    MEDF_ES_CODEC_MP2,
    MEDF_ES_CODEC_AAC,
} medf_es_codec_t;

typedef struct {
    medf_es_codec_t codec;
    uint32_t sample_rate;
    uint32_t bitrate_bps;
    uint8_t channels;
    int64_t duration_us;
    bool duration_exact;
    medf_tags_t tags;
} medf_es_info_t;

typedef struct {
    int64_t pts_us;
    const uint8_t *data;
    uint32_t size;
    uint32_t ref;
} medf_es_packet_t;

typedef struct medf_es_demux medf_es_demux_t;

/* `want_cover` false skips reading any embedded picture; the tags still say
   where it is. */
medf_es_demux_t *medf_es_demux_open(const char *path, const medf_arena_t *arena, bool want_cover,
                                    const char **error);
void medf_es_demux_close(medf_es_demux_t *demux);

const medf_es_info_t *medf_es_demux_info(const medf_es_demux_t *demux);
medf_buffer_t *medf_es_demux_buffer(medf_es_demux_t *demux);

bool medf_es_demux_read(medf_es_demux_t *demux, medf_es_packet_t *packet);
bool medf_es_demux_seek(medf_es_demux_t *demux, int64_t pts_us, int64_t *landed_us);

#ifdef __cplusplus
}
#endif
