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

#define MEDF_OGG_OPUS_HEAD_BYTES 19

typedef struct {
    uint32_t sample_rate; /* 48000, the rate Opus decodes at */
    uint32_t bitrate_bps;
    uint8_t channels;
    uint8_t opus_head[MEDF_OGG_OPUS_HEAD_BYTES]; /* without a channel mapping table */
    int64_t duration_us;
    medf_tags_t tags;
} medf_ogg_info_t;

typedef struct {
    int64_t pts_us;
    const uint8_t *data;
    uint32_t size;
    uint32_t ref;
} medf_ogg_packet_t;

typedef struct medf_ogg_demux medf_ogg_demux_t;

/* Ogg Opus (RFC 7845), the first logical stream of the file only. */
medf_ogg_demux_t *medf_ogg_demux_open(const char *path, const medf_arena_t *arena,
                                      bool want_cover, const char **error);
void medf_ogg_demux_close(medf_ogg_demux_t *demux);

const medf_ogg_info_t *medf_ogg_demux_info(const medf_ogg_demux_t *demux);
medf_buffer_t *medf_ogg_demux_buffer(medf_ogg_demux_t *demux);

/* Each packet is one Opus packet, timed from 0 after the pre-skip, so the
 * first ones are early. A packet that spans pages is a copy the demuxer keeps
 * until it is released here rather than through the buffer. */
bool medf_ogg_demux_read(medf_ogg_demux_t *demux, medf_ogg_packet_t *packet);
void medf_ogg_demux_release(medf_ogg_demux_t *demux, uint32_t ref);
void medf_ogg_demux_release_all(medf_ogg_demux_t *demux);
/* Lands on the start of the packet that holds `pts_us`. */
bool medf_ogg_demux_seek(medf_ogg_demux_t *demux, int64_t pts_us, int64_t *landed_us);

#ifdef __cplusplus
}
#endif
