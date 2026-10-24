/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    MEDF_RIFF_AUDIO_CODEC_NONE,
    MEDF_RIFF_AUDIO_CODEC_PCM,
    MEDF_RIFF_AUDIO_CODEC_MP3,
    MEDF_RIFF_AUDIO_CODEC_MP2,
    MEDF_RIFF_AUDIO_CODEC_ADPCM_IMA,
    MEDF_RIFF_AUDIO_CODEC_AAC,
    MEDF_RIFF_AUDIO_CODEC_UNSUPPORTED,
} medf_riff_audio_codec_t;

const char *medf_riff_audio_codec_name(medf_riff_audio_codec_t codec);

#ifdef __cplusplus
}
#endif
