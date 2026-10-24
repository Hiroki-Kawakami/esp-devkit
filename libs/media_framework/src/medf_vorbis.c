/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdlib.h>
#include <string.h>
#include <strings.h>

#include "medf_tags.h"

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

bool medf_tags_read_vorbis_comment(medf_buffer_t *reader, off_t offset, uint32_t size,
                                   medf_tags_t *tags) {
    const off_t end = offset + size;
    uint8_t word[4];
    if (size < 8 || !read_exact(reader, offset, word, 4)) return false;
    off_t at = offset + 4 + (off_t)le32(word);
    if (at + 4 > end || !read_exact(reader, at, word, 4)) return false;
    uint32_t count = le32(word);
    at += 4;

    char track[MEDF_TAG_SHORT_BYTES] = "";
    uint32_t total = 0;
    char text[VORBIS_COMMENT_PEEK_BYTES];
    for (; count && at + 4 <= end; count--) {
        if (!read_exact(reader, at, word, 4)) return false;
        const uint32_t len = le32(word);
        at += 4;
        if (len > (uint64_t)(end - at)) return false;
        const size_t peek = len < sizeof(text) ? len : sizeof(text);
        if (!read_exact(reader, at, text, peek)) return false;
        at += len;

        const char *eq = memchr(text, '=', peek);
        if (!eq) continue;
        const size_t key_len = (size_t)(eq - text);
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
