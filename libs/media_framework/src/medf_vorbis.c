/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdlib.h>
#include <string.h>
#include <strings.h>

#include "medf_tags.h"

#include "esp_heap_caps.h"

#define VORBIS_COMMENT_PEEK_BYTES 256

static bool read_exact(medf_buffer_t *reader, off_t at, void *out, size_t size) {
    medf_buffer_seek(reader, at);
    return medf_buffer_read(reader, out, size) == size;
}

static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static uint32_t be32(const uint8_t *p) {
    return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3];
}

static bool field_for(const char *key, size_t len, medf_tag_field_t *field) {
    static const struct {
        const char *key;
        medf_tag_field_t field;
    } kKeys[] = {
        { "TITLE", MEDF_TAG_TITLE },
        { "ARTIST", MEDF_TAG_ARTIST },
        { "ALBUM", MEDF_TAG_ALBUM },
        { "ALBUMARTIST", MEDF_TAG_ALBUM_ARTIST },
        { "ALBUM ARTIST", MEDF_TAG_ALBUM_ARTIST },
        { "DATE", MEDF_TAG_DATE },
        { "YEAR", MEDF_TAG_DATE },
    };
    for (size_t i = 0; i < sizeof(kKeys) / sizeof(kKeys[0]); i++) {
        if (strlen(kKeys[i].key) == len && strncasecmp(key, kKeys[i].key, len) == 0) {
            *field = kKeys[i].field;
            return true;
        }
    }
    return false;
}

static bool key_is(const char *key, size_t len, const char *name) {
    return strlen(name) == len && strncasecmp(key, name, len) == 0;
}

static int base64_value(uint8_t c) {
    if (c >= 'A' && c <= 'Z') return c - 'A';
    if (c >= 'a' && c <= 'z') return c - 'a' + 26;
    if (c >= '0' && c <= '9') return c - '0' + 52;
    if (c == '+') return 62;
    if (c == '/') return 63;
    return -1;
}

/* Decodes base64 in place and returns the byte count. */
static size_t base64_decode(uint8_t *data, size_t size) {
    uint32_t bits = 0;
    int count = 0;
    size_t out = 0;
    for (size_t i = 0; i < size; i++) {
        const int v = base64_value(data[i]);
        if (v < 0) continue;
        bits = bits << 6 | (uint32_t)v;
        count += 6;
        if (count >= 8) {
            count -= 8;
            data[out++] = (uint8_t)(bits >> count);
        }
    }
    return out;
}

/* A FLAC PICTURE block in memory, as METADATA_BLOCK_PICTURE carries it. */
static void adopt_picture_block(medf_tags_t *tags, uint8_t *owner, uint8_t *block, size_t size) {
    if (size < 32) {
        heap_caps_free(owner);
        return;
    }
    const bool front = be32(block) == 3;
    size_t at = 8 + (size_t)be32(block + 4);
    if (at + 4 > size) {
        heap_caps_free(owner);
        return;
    }
    at += 4 + (size_t)be32(block + at);
    if (at + 20 > size) {
        heap_caps_free(owner);
        return;
    }
    const uint32_t length = be32(block + at + 16);
    at += 20;
    if (!length || length > size - at) {
        heap_caps_free(owner);
        return;
    }
    medf_tags_adopt_cover(tags, owner, block + at, length, front);
}

typedef struct {
    medf_buffer_t *reader;
    off_t at;
} file_stream_t;

static bool file_read(void *ctx, void *out, size_t size) {
    file_stream_t *f = ctx;
    if (!read_exact(f->reader, f->at, out, size)) return false;
    f->at += (off_t)size;
    return true;
}

static bool file_skip(void *ctx, size_t size) {
    ((file_stream_t *)ctx)->at += (off_t)size;
    return true;
}

bool medf_tags_read_vorbis_comment(medf_buffer_t *reader, off_t offset, uint32_t size,
                                   medf_tags_t *tags) {
    file_stream_t file = { reader, offset };
    const medf_stream_t in = { &file, file_read, file_skip };
    return medf_tags_read_vorbis_comment_stream(&in, size, tags, false);
}

bool medf_tags_read_vorbis_comment_stream(const medf_stream_t *in, uint32_t size, medf_tags_t *tags,
                                          bool want_cover) {
    uint32_t left = size;
    uint8_t word[4];
    if (left < 8 || !in->read(in->ctx, word, 4)) return false;
    left -= 4;
    const uint32_t vendor = le32(word);
    if (vendor > left - 4 || !in->skip(in->ctx, vendor)) return false;
    left -= vendor;
    if (!in->read(in->ctx, word, 4)) return false;
    left -= 4;
    uint32_t count = le32(word);

    char track[MEDF_TAG_SHORT_BYTES] = "";
    uint32_t total = 0;
    char text[VORBIS_COMMENT_PEEK_BYTES];
    for (; count && left >= 4; count--) {
        if (!in->read(in->ctx, word, 4)) return false;
        left -= 4;
        const uint32_t len = le32(word);
        if (len > left) return false;
        left -= len;
        const size_t peek = len < sizeof(text) ? len : sizeof(text);
        if (!in->read(in->ctx, text, peek)) return false;

        const char *eq = memchr(text, '=', peek);
        const size_t key_len = eq ? (size_t)(eq - text) : 0;
        if (eq && key_is(text, key_len, "METADATA_BLOCK_PICTURE")) {
            const size_t encoded = len - key_len - 1;
            uint8_t *owner = NULL;
            if (want_cover && encoded / 4 * 3 <= MEDF_COVER_MAX_BYTES + 1024) {
                owner = heap_caps_malloc(encoded, MALLOC_CAP_SPIRAM);
            }
            if (owner) {
                const size_t first = peek - key_len - 1;
                memcpy(owner, eq + 1, first);
                if (!in->read(in->ctx, owner + first, encoded - first)) {
                    heap_caps_free(owner);
                    return false;
                }
                adopt_picture_block(tags, owner, owner, base64_decode(owner, encoded));
                continue;
            }
        }
        if (len > peek && !in->skip(in->ctx, len - peek)) return false;
        if (!eq) continue;
        const char *value = eq + 1;
        const size_t value_len = peek - key_len - 1;
        medf_tag_field_t field;
        if (field_for(text, key_len, &field)) {
            medf_tags_set(tags, field, value, value_len, MEDF_TEXT_UTF8);
        } else if (key_is(text, key_len, "TRACKNUMBER") && !track[0]) {
            const size_t n = value_len < sizeof(track) - 1 ? value_len : sizeof(track) - 1;
            memcpy(track, value, n);
            track[n] = '\0';
        } else if (key_is(text, key_len, "TRACKTOTAL") || key_is(text, key_len, "TOTALTRACKS")) {
            char digits[12] = "";
            memcpy(digits, value, value_len < sizeof(digits) - 1 ? value_len : sizeof(digits) - 1);
            if (!total) total = (uint32_t)strtoul(digits, NULL, 10);
        }
    }
    if (track[0] && total && !strchr(track, '/')) {
        medf_tags_set_number(tags, MEDF_TAG_TRACK, (uint32_t)strtoul(track, NULL, 10), total);
    } else if (track[0]) {
        medf_tags_set(tags, MEDF_TAG_TRACK, track, strlen(track), MEDF_TEXT_UTF8);
    }
    return true;
}

bool medf_tags_read_flac_picture(medf_buffer_t *reader, off_t offset, uint32_t size,
                                 medf_tags_t *tags, bool want_cover, bool *have_front) {
    const off_t end = offset + size;
    uint8_t head[8];
    if (size < 32 || !read_exact(reader, offset, head, 8)) return false;
    const bool front = be32(head) == 3;
    off_t at = offset + 8 + (off_t)be32(head + 4);
    if (at + 4 > end || !read_exact(reader, at, head, 4)) return false;
    at += 4 + (off_t)be32(head);
    uint8_t dims[20];
    if (at + 20 > end || !read_exact(reader, at, dims, sizeof(dims))) return false;
    at += 20;
    const uint32_t length = be32(dims + 16);
    if (!length || length > (uint64_t)(end - at)) return false;

    uint8_t magic[8];
    if (length >= sizeof(magic) && read_exact(reader, at, magic, sizeof(magic)) &&
        medf_cover_format_of(magic, sizeof(magic)) == MEDF_COVER_NONE) {
        return true;
    }
    if (tags->cover_at.size && (!front || *have_front)) return true;
    tags->cover_at.offset = at;
    tags->cover_at.size = length;
    *have_front = front;
    if (!want_cover) return true;

    void *owner = NULL;
    uint8_t *data = medf_buffer_read_alloc(reader, at, length, &owner);
    if (data) medf_tags_adopt_cover(tags, owner, data, length, front);
    return true;
}
