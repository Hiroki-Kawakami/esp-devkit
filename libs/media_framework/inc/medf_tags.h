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

#ifdef __cplusplus
extern "C" {
#endif

#define MEDF_TAG_TEXT_BYTES 128
#define MEDF_TAG_SHORT_BYTES 16
#define MEDF_COVER_MAX_BYTES (2 * 1024 * 1024)

typedef enum {
    MEDF_TAG_TITLE,
    MEDF_TAG_ARTIST,
    MEDF_TAG_ALBUM,
    MEDF_TAG_ALBUM_ARTIST,
    MEDF_TAG_TRACK,
    MEDF_TAG_DATE,
    MEDF_TAG_COUNT,
} medf_tag_field_t;

typedef enum {
    MEDF_TEXT_LATIN1,
    MEDF_TEXT_UTF16,
    MEDF_TEXT_UTF16BE,
    MEDF_TEXT_UTF8,
} medf_text_encoding_t;

typedef enum {
    MEDF_COVER_NONE,
    MEDF_COVER_JPEG,
    MEDF_COVER_PNG,
} medf_cover_format_t;

typedef struct {
    uint8_t *owner;   /* allocation to free; `data` points into it */
    uint8_t *data;
    uint32_t size;
    medf_cover_format_t format;
    bool front;
} medf_cover_t;

/* Where the picture sits in the file. Filled even when the body was skipped,
   so a later pass can read just those bytes instead of the whole tag. */
typedef struct {
    off_t offset;
    uint32_t size;
} medf_cover_at_t;

typedef struct {
    char title[MEDF_TAG_TEXT_BYTES];
    char artist[MEDF_TAG_TEXT_BYTES];
    char album[MEDF_TAG_TEXT_BYTES];
    char album_artist[MEDF_TAG_TEXT_BYTES];
    char track[MEDF_TAG_SHORT_BYTES];
    char date[MEDF_TAG_SHORT_BYTES];
    medf_cover_t cover;
    medf_cover_at_t cover_at;
    /* The pictures were enumerated, so an empty `cover`/`cover_at` means the
       file has none rather than that nobody looked. */
    bool cover_scanned;
} medf_tags_t;

const char *medf_tags_get(const medf_tags_t *tags, medf_tag_field_t field);
bool medf_tags_empty(const medf_tags_t *tags);

/* Converts to UTF-8 and stores it; the first non-empty value of a field wins,
 * so parsers may walk from the most to the least preferred source. */
void medf_tags_set(medf_tags_t *tags, medf_tag_field_t field, const void *bytes, size_t size,
                   medf_text_encoding_t encoding);
void medf_tags_set_number(medf_tags_t *tags, medf_tag_field_t field, uint32_t value,
                          uint32_t total);

/* Keeps a copy of the picture when it is a JPEG or a PNG within the size
 * limit. A front cover replaces a picture stored without that hint. */
void medf_tags_set_cover(medf_tags_t *tags, const void *data, size_t size, bool front);

/* JPEG/PNG magic, so a caller can tell a picture from any other payload. */
medf_cover_format_t medf_cover_format_of(const void *data, size_t size);

/* Same rules, but takes ownership of `owner` instead of copying: `data`/`size`
   describe the picture inside it. `owner` is freed when the picture is
   rejected, so the caller hands it over either way. */
void medf_tags_adopt_cover(medf_tags_t *tags, uint8_t *owner, uint8_t *data, size_t size,
                           bool front);

/* Hands the picture buffer to the caller; the tag stops owning it. */
medf_cover_t medf_tags_take_cover(medf_tags_t *tags);

void medf_tags_free(medf_tags_t *tags);

/* Reads the ID3v2 tag at `offset`, if any. `*end` is left alone when there is
 * no readable tag, so a caller can use it as the start of the audio data.
 * With `want_cover` false the picture body is located but never read, which is
 * nearly the whole cost of a tag on a file with cover art; `tags->cover_at`
 * then says where to find it later. */
bool medf_tags_read_id3v2(medf_buffer_t *reader, off_t offset, off_t *end, medf_tags_t *tags,
                          bool want_cover);

/* `end` is the end of the 128 byte ID3v1 block, i.e. the end of the file. */
bool medf_tags_read_id3v1(medf_buffer_t *reader, off_t end, medf_tags_t *tags);

/* A sequential byte source, for tags whose bytes are not contiguous in the
 * file. */
typedef struct {
    void *ctx;
    bool (*read)(void *ctx, void *out, size_t size);
    bool (*skip)(void *ctx, size_t size);
} medf_stream_t;

/* Reads a Vorbis comment body of `size` bytes at `offset`. */
bool medf_tags_read_vorbis_comment(medf_buffer_t *reader, off_t offset, uint32_t size,
                                   medf_tags_t *tags);
/* The same from a stream; with `want_cover` a METADATA_BLOCK_PICTURE (a base64
 * FLAC PICTURE block) becomes the cover. */
bool medf_tags_read_vorbis_comment_stream(const medf_stream_t *in, uint32_t size, medf_tags_t *tags,
                                          bool want_cover);

/* Reads a FLAC PICTURE body, with the same `want_cover` rule as ID3v2.
 * `have_front` carries across the pictures of one file and starts false. */
bool medf_tags_read_flac_picture(medf_buffer_t *reader, off_t offset, uint32_t size,
                                 medf_tags_t *tags, bool want_cover, bool *have_front);

#ifdef __cplusplus
}
#endif
