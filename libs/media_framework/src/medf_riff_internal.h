/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#include "medf_buffer.h"
#include "medf_tags.h"
#include "medf_riff.h"
#include "medf_riff_format.h"

typedef struct {
    medf_riff_audio_codec_t codec;
    uint32_t sample_rate;
    uint32_t bitrate_bps;
    uint8_t channels;
    uint8_t bits_per_sample;
    uint16_t block_align;
    uint8_t *extra;
    uint32_t extra_size;
} riff_audio_format_t;

bool medf_riff_read(medf_buffer_t *reader, void *out, size_t size);

/* Reads the next chunk header below `limit` and reports where its body starts
 * and where the following chunk does (the pad byte on an odd size included). */
bool medf_riff_next_chunk(medf_buffer_t *reader, off_t limit, riff_chunk_t *chunk, off_t *body,
                          off_t *next);

medf_riff_audio_codec_t medf_riff_audio_codec_of_tag(uint16_t format_tag);

/* Reads a WAVEFORMATEX body of `chunk_size` bytes from the current position.
 * `out->extra` is heap memory the caller frees. */
bool medf_riff_read_wave_format(medf_buffer_t *reader, uint32_t chunk_size, riff_audio_format_t *out);

/* Reads the chunks of a LIST INFO whose type fourcc the caller has already
 * consumed, up to `end`. */
void medf_riff_read_info(medf_buffer_t *reader, off_t end, medf_tags_t *tags);
