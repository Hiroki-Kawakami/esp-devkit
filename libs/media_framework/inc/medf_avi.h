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

typedef enum {
    MEDF_AVI_VIDEO_CODEC_NONE,
    MEDF_AVI_VIDEO_CODEC_MJPEG,
    MEDF_AVI_VIDEO_CODEC_H264,
    MEDF_AVI_VIDEO_CODEC_MPEG2,
    MEDF_AVI_VIDEO_CODEC_UNSUPPORTED,
} medf_avi_video_codec_t;

typedef struct {
    struct {
        medf_avi_video_codec_t codec;
        uint32_t width;
        uint32_t height;
        uint32_t frame_count;
        uint32_t frame_interval_us;
        uint32_t max_frame_bytes;
        const uint8_t *codec_private;
        uint32_t codec_private_size;
    } video;
    struct {
        medf_riff_audio_codec_t codec;
        uint32_t sample_rate;
        uint32_t bitrate_bps;
        uint32_t max_frame_bytes;
        uint8_t channels;
        uint8_t bits_per_sample;
        uint16_t block_align;
        const uint8_t *codec_private;
        uint32_t codec_private_size;
    } audio;
    bool seekable;
    medf_tags_t tags;
} medf_avi_info_t;

typedef enum {
    MEDF_AVI_PACKET_VIDEO,
    MEDF_AVI_PACKET_AUDIO,
} medf_avi_packet_type_t;

typedef struct {
    medf_avi_packet_type_t type;
    const uint8_t *data;
    uint32_t size;
    uint32_t ref;
    uint32_t frame_index;
    bool keyframe;
} medf_avi_packet_t;

typedef struct medf_avi_demux medf_avi_demux_t;

medf_avi_demux_t *medf_avi_demux_open(const char *path, const medf_arena_t *arena, bool want_cover,
                                      const char **error);
void medf_avi_demux_close(medf_avi_demux_t *demux);

const medf_avi_info_t *medf_avi_demux_info(const medf_avi_demux_t *demux);

medf_buffer_t *medf_avi_demux_buffer(medf_avi_demux_t *demux);

bool medf_avi_demux_read(medf_avi_demux_t *demux, medf_avi_packet_t *packet, bool want_audio);

bool medf_avi_demux_seek(medf_avi_demux_t *demux, uint32_t frame, uint32_t *landed_frame);
bool medf_avi_demux_keyframe_before(const medf_avi_demux_t *demux, uint32_t frame, uint32_t *key_frame);
/* Where the last video keyframe at or before `frame` sits in the file. Moves
   the read position: seek before reading packets again. */
bool medf_avi_demux_keyframe_at(medf_avi_demux_t *demux, uint32_t frame, uint64_t *offset, uint32_t *size);

#ifdef __cplusplus
}
#endif
