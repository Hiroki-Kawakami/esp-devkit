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
    MEDF_MKV_VIDEO_CODEC_NONE,
    MEDF_MKV_VIDEO_CODEC_MJPEG,
    MEDF_MKV_VIDEO_CODEC_H264,
    MEDF_MKV_VIDEO_CODEC_MPEG2,
    MEDF_MKV_VIDEO_CODEC_UNSUPPORTED,
} medf_mkv_video_codec_t;

typedef enum {
    MEDF_MKV_AUDIO_CODEC_NONE,
    MEDF_MKV_AUDIO_CODEC_PCM,
    MEDF_MKV_AUDIO_CODEC_MP3,
    MEDF_MKV_AUDIO_CODEC_MP2,
    MEDF_MKV_AUDIO_CODEC_ADPCM_IMA,
    MEDF_MKV_AUDIO_CODEC_AAC,
    MEDF_MKV_AUDIO_CODEC_OPUS,
    MEDF_MKV_AUDIO_CODEC_FLAC,
    MEDF_MKV_AUDIO_CODEC_ALAC,
    MEDF_MKV_AUDIO_CODEC_UNSUPPORTED,
} medf_mkv_audio_codec_t;

typedef struct {
    struct {
        medf_mkv_video_codec_t codec;
        uint32_t width;
        uint32_t height;
        int64_t frame_interval_us;
        uint16_t rotation_ccw;
        const uint8_t *codec_private;
        uint32_t codec_private_size;
    } video;
    struct {
        medf_mkv_audio_codec_t codec;
        uint32_t sample_rate;
        uint8_t channels;
        uint8_t bits_per_sample;
        uint16_t block_align;
        const uint8_t *codec_private;
        uint32_t codec_private_size;
    } audio;
    int64_t duration_us;
    bool seekable;
    medf_tags_t tags;
} medf_mkv_info_t;

typedef enum {
    MEDF_MKV_PACKET_VIDEO,
    MEDF_MKV_PACKET_AUDIO,
} medf_mkv_packet_type_t;

typedef struct {
    medf_mkv_packet_type_t type;
    int64_t pts_us;
    bool keyframe;
    const uint8_t *data;
    uint32_t size;
    uint32_t ref;
} medf_mkv_packet_t;

typedef struct medf_mkv_demux medf_mkv_demux_t;

medf_mkv_demux_t *medf_mkv_demux_open(const char *path, const medf_arena_t *arena, bool want_cover,
                                      const char **error);
void medf_mkv_demux_close(medf_mkv_demux_t *demux);

const medf_mkv_info_t *medf_mkv_demux_info(const medf_mkv_demux_t *demux);
medf_buffer_t *medf_mkv_demux_buffer(medf_mkv_demux_t *demux);

bool medf_mkv_demux_read(medf_mkv_demux_t *demux, medf_mkv_packet_t *packet, bool want_audio);
bool medf_mkv_demux_seek(medf_mkv_demux_t *demux, int64_t pts_us, int64_t *landed_us);
bool medf_mkv_demux_keyframe_before(const medf_mkv_demux_t *demux, int64_t pts_us, int64_t *key_us);
/* Where the first video keyframe of the cue at or before `pts_us` sits in the
   file. Moves the read position: seek before reading packets again. */
bool medf_mkv_demux_keyframe_at(medf_mkv_demux_t *demux, int64_t pts_us, uint64_t *offset, uint32_t *size);

#ifdef __cplusplus
}
#endif
