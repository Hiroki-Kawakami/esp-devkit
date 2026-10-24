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

#define MEDF_FLAC_STREAMINFO_BYTES 34

typedef struct {
    uint32_t sample_rate;
    uint32_t bitrate_bps;
    uint8_t channels;
    uint8_t bits_per_sample;
    uint8_t streaminfo[MEDF_FLAC_STREAMINFO_BYTES]; /* body without the block header */
    int64_t duration_us;
    medf_tags_t tags;
} medf_flac_info_t;

typedef struct {
    int64_t pts_us;
    const uint8_t *data;
    uint32_t size;
    uint32_t ref;
} medf_flac_packet_t;

typedef struct medf_flac_demux medf_flac_demux_t;

/* The STREAMINFO body in a list of metadata blocks, with or without "fLaC" in
 * front (Matroska CodecPrivate, the blocks of an MP4 dfLa); NULL without one. */
const uint8_t *medf_flac_find_streaminfo(const uint8_t *blocks, size_t size);
void medf_flac_streaminfo_format(const uint8_t *streaminfo, uint32_t *sample_rate,
                                 uint8_t *channels, uint8_t *bits_per_sample);

/* `want_cover` false skips reading any embedded picture; the tags still say
   where it is. */
medf_flac_demux_t *medf_flac_demux_open(const char *path, const medf_arena_t *arena,
                                        bool want_cover, const char **error);
void medf_flac_demux_close(medf_flac_demux_t *demux);

const medf_flac_info_t *medf_flac_demux_info(const medf_flac_demux_t *demux);
medf_buffer_t *medf_flac_demux_buffer(medf_flac_demux_t *demux);

/* Each packet is one whole frame. */
bool medf_flac_demux_read(medf_flac_demux_t *demux, medf_flac_packet_t *packet);
/* Lands on the start of the frame that holds `pts_us`. */
bool medf_flac_demux_seek(medf_flac_demux_t *demux, int64_t pts_us, int64_t *landed_us);

#ifdef __cplusplus
}
#endif
