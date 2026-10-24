/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "medf_flac.h"

#include <stdlib.h>
#include <string.h>

#include "esp_heap_caps.h"
#include "esp_log.h"

static const char *TAG = "medf_flac_demux";

#define FLAC_HEADER_MAX_BYTES 16
#define FLAC_SCAN_BYTES 4096
#define FLAC_WINDOW_BYTES (16 * 1024)
#define FLAC_MAX_SEEK_POINTS 4096
#define FLAC_SEEK_STEPS 32

enum {
    BLOCK_STREAMINFO = 0,
    BLOCK_SEEKTABLE = 3,
    BLOCK_VORBIS_COMMENT = 4,
    BLOCK_PICTURE = 6,
};

typedef struct {
    uint64_t sample;
    uint32_t block;
    uint8_t length;
} header_t;

typedef struct {
    off_t offset;
    header_t h;
} frame_t;

typedef struct {
    uint64_t sample;
    uint64_t offset;
} seek_point_t;

struct medf_flac_demux {
    medf_buffer_t *reader;
    medf_flac_info_t info;
    off_t data_start;
    off_t data_end;
    off_t cursor;
    size_t max_view;
    uint64_t total_samples;
    uint32_t max_block;
    uint32_t max_frame;
    uint32_t nominal_block;
    bool variable;
    bool strategy_known;
    bool sample_numbered;
    seek_point_t *points;
    uint32_t point_count;
    uint8_t scan[FLAC_SCAN_BYTES];
};

static uint32_t be16(const uint8_t *p) {
    return (uint32_t)p[0] << 8 | p[1];
}

static uint32_t be24(const uint8_t *p) {
    return (uint32_t)p[0] << 16 | (uint32_t)p[1] << 8 | p[2];
}

static uint64_t be64(const uint8_t *p) {
    uint64_t v = 0;
    for (int i = 0; i < 8; i++) v = v << 8 | p[i];
    return v;
}

static bool read_exact(medf_flac_demux_t *demux, off_t at, void *out, size_t size) {
    medf_buffer_seek(demux->reader, at);
    return medf_buffer_read(demux->reader, out, size) == size;
}

static uint8_t crc8(const uint8_t *p, size_t n) {
    uint8_t crc = 0;
    while (n--) {
        crc ^= *p++;
        for (int b = 0; b < 8; b++) crc = (uint8_t)(crc & 0x80 ? crc << 1 ^ 0x07 : crc << 1);
    }
    return crc;
}

/* A frame header that agrees with STREAMINFO and its own CRC-8. Sync words
 * turn up in compressed data, so nothing less is trusted. */
static bool parse_header(const medf_flac_demux_t *demux, const uint8_t *p, size_t avail,
                         header_t *h) {
    static const uint32_t kRates[12] = { 0,     88200, 176400, 192000, 8000,  16000,
                                         22050, 24000, 32000,  44100,  48000, 96000 };
    static const uint8_t kSizes[8] = { 0, 8, 12, 0, 16, 20, 24, 32 };

    if (avail < 6 || p[0] != 0xFF || (p[1] & 0xFE) != 0xF8) return false;
    const bool variable = p[1] & 1;
    if (demux->strategy_known && variable != demux->variable) return false;
    const uint8_t block_code = p[2] >> 4;
    const uint8_t rate_code = p[2] & 0x0F;
    const uint8_t assignment = p[3] >> 4;
    const uint8_t size_code = (p[3] >> 1) & 7;
    if (block_code == 0 || rate_code == 15 || assignment > 10 || (p[3] & 1)) return false;
    if ((assignment < 8 ? assignment + 1 : 2) != demux->info.channels) return false;
    if (size_code == 3 || (size_code && kSizes[size_code] != demux->info.bits_per_sample)) {
        return false;
    }

    int ones = 0;
    while (ones < 8 && (p[4] & (0x80 >> ones))) ones++;
    if (ones == 1 || ones == 8) return false;
    const size_t coded = ones ? (size_t)ones : 1;
    if (avail < 4 + coded + 5) return false;
    uint64_t number = p[4] & (0x7F >> ones);
    for (size_t i = 1; i < coded; i++) {
        if ((p[4 + i] & 0xC0) != 0x80) return false;
        number = number << 6 | (p[4 + i] & 0x3F);
    }
    size_t i = 4 + coded;

    uint32_t block;
    if (block_code == 1) {
        block = 192;
    } else if (block_code <= 5) {
        block = 576u << (block_code - 2);
    } else if (block_code == 6) {
        block = p[i++] + 1u;
    } else if (block_code == 7) {
        block = be16(p + i) + 1;
        i += 2;
    } else {
        block = 256u << (block_code - 8);
    }

    uint32_t rate = demux->info.sample_rate;
    if (rate_code >= 1 && rate_code <= 11) rate = kRates[rate_code];
    if (rate_code == 12) rate = p[i++] * 1000u;
    if (rate_code == 13 || rate_code == 14) {
        rate = be16(p + i) * (rate_code == 14 ? 10u : 1u);
        i += 2;
    }
    if (rate != demux->info.sample_rate || block > demux->max_block) return false;
    if (i >= avail || crc8(p, i) != p[i]) return false;

    h->block = block;
    h->length = (uint8_t)(i + 1);
    h->sample = variable || demux->sample_numbered ? number : number * demux->nominal_block;
    return true;
}

/* First header in p[from, usable) that parses (and, with `expect`, starts at
 * that sample); bytes up to `len` may complete it. */
static bool scan_span(const medf_flac_demux_t *demux, const uint8_t *p, size_t len, size_t from,
                      size_t usable, const uint64_t *expect, size_t *at, header_t *h) {
    for (size_t i = from; i < usable; i++) {
        if (p[i] != 0xFF) continue;
        if (parse_header(demux, p + i, len - i, h) && (!expect || h->sample == *expect)) {
            *at = i;
            return true;
        }
    }
    return false;
}

static bool find_header(medf_flac_demux_t *demux, off_t from, off_t limit, const uint64_t *expect,
                        frame_t *out) {
    if (limit > demux->data_end) limit = demux->data_end;
    off_t at = from;
    while (at < limit) {
        const size_t want = (size_t)(demux->data_end - at) < FLAC_SCAN_BYTES
                                ? (size_t)(demux->data_end - at) : FLAC_SCAN_BYTES;
        medf_buffer_seek(demux->reader, at);
        const size_t got = medf_buffer_read(demux->reader, demux->scan, want);
        if (got < 2) return false;
        const bool last = at + (off_t)got >= demux->data_end;
        size_t usable = last ? got : got - (FLAC_HEADER_MAX_BYTES - 1);
        if (at + (off_t)usable > limit) usable = (size_t)(limit - at);
        size_t i;
        if (scan_span(demux, demux->scan, got, 0, usable, expect, &i, &out->h)) {
            out->offset = at + (off_t)i;
            return true;
        }
        if (last) return false;
        at += (off_t)usable;
    }
    return false;
}

/* No frame is larger than its samples stored verbatim, which holds even where
 * STREAMINFO's maximum frame size is wrong. */
static off_t frame_reach(const medf_flac_demux_t *demux) {
    const uint64_t verbatim = (uint64_t)demux->max_block * demux->info.channels *
                                  (demux->info.bits_per_sample + 1u) / 8 + 2 * FLAC_HEADER_MAX_BYTES;
    return verbatim < demux->max_view ? (off_t)verbatim : (off_t)demux->max_view;
}

/* The frame after `f`, found by the sample it has to start at. False at the
 * end of the data, and when nothing follows within reach. */
static bool next_frame(medf_flac_demux_t *demux, const frame_t *f, frame_t *out, bool *at_end) {
    const uint64_t expect = f->h.sample + f->h.block;
    const off_t limit = f->offset + frame_reach(demux);
    if (find_header(demux, f->offset + f->h.length, limit, &expect, out)) return true;
    *at_end = limit >= demux->data_end;
    return false;
}

static bool frame_at_or_after(medf_flac_demux_t *demux, off_t from, off_t limit, frame_t *out) {
    while (from < limit && find_header(demux, from, limit, NULL, out)) {
        frame_t next;
        bool at_end = false;
        if (next_frame(demux, out, &next, &at_end) || at_end) return true;
        from = out->offset + 1;
    }
    return false;
}

static bool read_blocks(medf_flac_demux_t *demux, off_t pos, bool want_cover, const char **error) {
    bool have_streaminfo = false, have_front = false;
    const off_t size = medf_buffer_size(demux->reader);
    for (bool last = false; !last;) {
        uint8_t head[4];
        if (!read_exact(demux, pos, head, sizeof(head))) {
            *error = "cannot read the FLAC header";
            return false;
        }
        last = head[0] & 0x80;
        const uint8_t type = head[0] & 0x7F;
        const uint32_t len = be24(head + 1);
        const off_t body = pos + 4;
        if (body + (off_t)len > size) {
            *error = "broken FLAC metadata";
            return false;
        }
        if (type == BLOCK_STREAMINFO && len >= MEDF_FLAC_STREAMINFO_BYTES) {
            have_streaminfo = read_exact(demux, body, demux->info.streaminfo, MEDF_FLAC_STREAMINFO_BYTES);
        } else if (type == BLOCK_SEEKTABLE && !demux->points && len >= 18) {
            uint32_t count = len / 18;
            if (count > FLAC_MAX_SEEK_POINTS) count = FLAC_MAX_SEEK_POINTS;
            demux->points = heap_caps_malloc(count * sizeof(seek_point_t), MALLOC_CAP_SPIRAM);
            for (uint32_t i = 0; demux->points && i < count; i++) {
                uint8_t point[18];
                if (!read_exact(demux, body + (off_t)i * 18, point, sizeof(point))) break;
                const uint64_t sample = be64(point);
                if (sample == UINT64_MAX) continue;
                demux->points[demux->point_count].sample = sample;
                demux->points[demux->point_count].offset = be64(point + 8);
                demux->point_count++;
            }
        } else if (type == BLOCK_VORBIS_COMMENT) {
            medf_tags_read_vorbis_comment(demux->reader, body, len, &demux->info.tags);
        } else if (type == BLOCK_PICTURE) {
            medf_tags_read_flac_picture(demux->reader, body, len, &demux->info.tags, want_cover,
                                        &have_front);
        }
        pos = body + len;
    }
    if (!have_streaminfo) {
        *error = "no STREAMINFO in this FLAC file";
        return false;
    }
    demux->data_start = pos;
    return true;
}

const uint8_t *medf_flac_find_streaminfo(const uint8_t *blocks, size_t size) {
    size_t pos = size >= 4 && memcmp(blocks, "fLaC", 4) == 0 ? 4 : 0;
    while (pos + 4 <= size) {
        const uint8_t type = blocks[pos] & 0x7F;
        const size_t len = be24(blocks + pos + 1);
        if (len > size - pos - 4) return NULL;
        if (type == BLOCK_STREAMINFO && len >= MEDF_FLAC_STREAMINFO_BYTES) return blocks + pos + 4;
        if (blocks[pos] & 0x80) return NULL;
        pos += 4 + len;
    }
    return NULL;
}

void medf_flac_streaminfo_format(const uint8_t *si, uint32_t *sample_rate, uint8_t *channels,
                                 uint8_t *bits_per_sample) {
    *sample_rate = (uint32_t)si[10] << 12 | (uint32_t)si[11] << 4 | si[12] >> 4;
    *channels = (uint8_t)(((si[12] >> 1) & 7) + 1);
    *bits_per_sample = (uint8_t)((((si[12] & 1) << 4) | si[13] >> 4) + 1);
}

static void apply_streaminfo(medf_flac_demux_t *demux) {
    const uint8_t *si = demux->info.streaminfo;
    demux->max_block = be16(si + 2);
    demux->max_frame = be24(si + 7);
    medf_flac_streaminfo_format(si, &demux->info.sample_rate, &demux->info.channels,
                                &demux->info.bits_per_sample);
    demux->total_samples = (uint64_t)(si[13] & 0x0F) << 32 | (uint32_t)si[14] << 24 |
                           (uint32_t)si[15] << 16 | (uint32_t)si[16] << 8 | si[17];
}

static bool find_first_frame(medf_flac_demux_t *demux, frame_t *first) {
    uint8_t head[FLAC_HEADER_MAX_BYTES];
    const size_t want = demux->data_end - demux->data_start < (off_t)sizeof(head)
                            ? (size_t)(demux->data_end - demux->data_start) : sizeof(head);
    if (want < 2 || !read_exact(demux, demux->data_start, head, want)) return false;
    demux->variable = head[1] & 1;
    demux->strategy_known = true;
    demux->nominal_block = 0;
    if (!parse_header(demux, head, want, &first->h)) {
        if (!find_header(demux, demux->data_start + 1, demux->data_start + frame_reach(demux), NULL,
                         first)) {
            return false;
        }
    } else {
        first->offset = demux->data_start;
    }
    demux->nominal_block = first->h.block;
    first->h.sample = 0;
    demux->data_start = first->offset;

    /* Old encoders wrote sample numbers under the fixed-size flag; the second
       frame's number tells the two apart. */
    frame_t next;
    bool at_end = false;
    if (!demux->variable && !next_frame(demux, first, &next, &at_end) && !at_end) {
        demux->sample_numbered = true;
        if (!next_frame(demux, first, &next, &at_end)) demux->sample_numbered = false;
    }
    return true;
}

static uint64_t count_samples(medf_flac_demux_t *demux) {
    off_t from = demux->data_end - 2 * frame_reach(demux);
    if (from < demux->data_start) from = demux->data_start;
    frame_t f;
    if (!frame_at_or_after(demux, from, demux->data_end, &f)) return 0;
    frame_t next;
    bool at_end = false;
    while (next_frame(demux, &f, &next, &at_end)) f = next;
    return f.h.sample + f.h.block;
}

medf_flac_demux_t *medf_flac_demux_open(const char *path, const medf_arena_t *arena,
                                        bool want_cover, const char **error) {
    const char *ignored = NULL;
    if (!error) error = &ignored;
    *error = NULL;

    medf_flac_demux_t *demux = heap_caps_calloc(1, sizeof(*demux), MALLOC_CAP_SPIRAM);
    if (!demux) {
        *error = "out of memory";
        return NULL;
    }
    demux->reader = medf_buffer_open(path, arena);
    if (!demux->reader) {
        *error = "cannot open the file";
        heap_caps_free(demux);
        return NULL;
    }
    demux->max_view = medf_arena_max_view(arena);
    demux->info.tags.cover_scanned = true;

    off_t start = 0;
    medf_tags_read_id3v2(demux->reader, 0, &start, &demux->info.tags, want_cover);
    uint8_t magic[4];
    if (!read_exact(demux, start, magic, sizeof(magic)) || memcmp(magic, "fLaC", 4) != 0) {
        *error = "not a FLAC file";
        medf_flac_demux_close(demux);
        return NULL;
    }
    if (!read_blocks(demux, start + 4, want_cover, error)) {
        medf_flac_demux_close(demux);
        return NULL;
    }
    apply_streaminfo(demux);
    if (!demux->info.sample_rate || !demux->max_block) {
        *error = "unusable FLAC STREAMINFO";
        medf_flac_demux_close(demux);
        return NULL;
    }

    demux->data_end = medf_buffer_size(demux->reader);
    if (demux->data_end - demux->data_start >= 128) {
        uint8_t tail[3];
        if (read_exact(demux, demux->data_end - 128, tail, 3) && memcmp(tail, "TAG", 3) == 0) {
            medf_tags_read_id3v1(demux->reader, demux->data_end, &demux->info.tags);
            demux->data_end -= 128;
        }
    }
    frame_t first;
    if (!find_first_frame(demux, &first)) {
        *error = "no FLAC frames in this file";
        medf_flac_demux_close(demux);
        return NULL;
    }
    for (uint32_t i = 0; i < demux->point_count; i++) {
        demux->points[i].offset += (uint64_t)demux->data_start;
    }

    if (!demux->total_samples) demux->total_samples = count_samples(demux);
    demux->info.duration_us =
        (int64_t)(demux->total_samples * 1000000ull / demux->info.sample_rate);
    demux->info.bitrate_bps =
        demux->info.duration_us > 0
            ? (uint32_t)((uint64_t)(demux->data_end - demux->data_start) * 8 * 1000000ull /
                         (uint64_t)demux->info.duration_us)
            : 0;

    ESP_LOGI(TAG, "%s: %u Hz x%u, %u bit, %lld us, %u seek points", path,
             (unsigned)demux->info.sample_rate, (unsigned)demux->info.channels,
             (unsigned)demux->info.bits_per_sample, (long long)demux->info.duration_us,
             (unsigned)demux->point_count);

    demux->cursor = demux->data_start;
    medf_buffer_seek(demux->reader, demux->cursor);
    medf_buffer_set_readahead(demux->reader, true);
    return demux;
}

void medf_flac_demux_close(medf_flac_demux_t *demux) {
    if (!demux) return;
    medf_tags_free(&demux->info.tags);
    if (demux->reader) medf_buffer_close(demux->reader);
    heap_caps_free(demux->points);
    heap_caps_free(demux);
}

const medf_flac_info_t *medf_flac_demux_info(const medf_flac_demux_t *demux) {
    return demux ? &demux->info : NULL;
}

medf_buffer_t *medf_flac_demux_buffer(medf_flac_demux_t *demux) {
    return demux ? demux->reader : NULL;
}

bool medf_flac_demux_read(medf_flac_demux_t *demux, medf_flac_packet_t *packet) {
    if (!demux || !packet) return false;

    while (demux->cursor < demux->data_end) {
        const size_t remaining = (size_t)(demux->data_end - demux->cursor);
        size_t window = demux->max_frame ? demux->max_frame + FLAC_HEADER_MAX_BYTES
                                         : FLAC_WINDOW_BYTES;
        if (window > demux->max_view) window = demux->max_view;
        bool resync = false;
        for (;;) {
            if (window > remaining) window = remaining;
            uint32_t ref = MEDF_NO_REF;
            const uint8_t *data = medf_buffer_view_at(demux->reader, demux->cursor, window, &ref);
            if (!data) return false;
            header_t h;
            if (!parse_header(demux, data, window, &h)) {
                medf_buffer_release(demux->reader, ref);
                resync = true;
                break;
            }
            const uint64_t expect = h.sample + h.block;
            size_t size = 0;
            header_t next;
            const size_t usable = window == remaining ? window : window - (FLAC_HEADER_MAX_BYTES - 1);
            if (!scan_span(demux, data, window, h.length, usable, &expect, &size, &next)) {
                if (window < remaining && window < demux->max_view) {
                    medf_buffer_release(demux->reader, ref);
                    window *= 2;
                    if (window > demux->max_view) window = demux->max_view;
                    continue;
                }
                if (window < remaining) {
                    medf_buffer_release(demux->reader, ref);
                    resync = true;
                    break;
                }
                size = window;
            }
            packet->pts_us = (int64_t)(h.sample * 1000000ull / demux->info.sample_rate);
            packet->data = data;
            packet->size = (uint32_t)size;
            packet->ref = ref;
            demux->cursor += (off_t)size;
            medf_buffer_seek(demux->reader, demux->cursor);
            return true;
        }
        if (resync) {
            frame_t f;
            if (!frame_at_or_after(demux, demux->cursor + 1, demux->data_end, &f)) {
                demux->cursor = demux->data_end;
                return false;
            }
            ESP_LOGW(TAG, "skipped %lld bytes to the next frame",
                     (long long)(f.offset - demux->cursor));
            demux->cursor = f.offset;
            medf_buffer_seek(demux->reader, demux->cursor);
        }
    }
    return false;
}

static bool frame_at(medf_flac_demux_t *demux, off_t offset, frame_t *out) {
    uint8_t head[FLAC_HEADER_MAX_BYTES];
    const off_t left = demux->data_end - offset;
    const size_t want = left < (off_t)sizeof(head) ? (size_t)left : sizeof(head);
    if (!want || !read_exact(demux, offset, head, want) || !parse_header(demux, head, want, &out->h)) {
        return false;
    }
    out->offset = offset;
    return true;
}

bool medf_flac_demux_seek(medf_flac_demux_t *demux, int64_t pts_us, int64_t *landed_us) {
    if (!demux) return false;
    const uint64_t target =
        pts_us > 0 ? (uint64_t)pts_us * demux->info.sample_rate / 1000000ull : 0;

    frame_t lo;
    if (!frame_at(demux, demux->data_start, &lo)) return false;
    off_t hi_offset = demux->data_end;
    uint64_t hi_sample = demux->total_samples;
    for (uint32_t i = 0; i < demux->point_count; i++) {
        const seek_point_t *p = &demux->points[i];
        frame_t f;
        if (p->sample <= target && p->sample > lo.h.sample && (off_t)p->offset < demux->data_end &&
            frame_at(demux, (off_t)p->offset, &f) && f.h.sample == p->sample) {
            lo = f;
        } else if (p->sample > target && (off_t)p->offset < hi_offset) {
            hi_offset = (off_t)p->offset;
            hi_sample = p->sample;
        }
    }

    const off_t linear = frame_reach(demux);
    for (int step = 0; step < FLAC_SEEK_STEPS && target >= lo.h.sample + lo.h.block; step++) {
        const off_t span = hi_offset - lo.offset;
        if (span <= linear || hi_sample <= lo.h.sample) break;
        off_t guess = lo.offset +
                      (off_t)((double)(target - lo.h.sample) / (double)(hi_sample - lo.h.sample) *
                              (double)span);
        if (guess <= lo.offset) guess = lo.offset + 1;
        if (guess >= hi_offset) guess = hi_offset - 1;
        frame_t f;
        if (!frame_at_or_after(demux, guess, hi_offset, &f)) {
            hi_offset = guess;
            continue;
        }
        if (f.h.sample > target) {
            hi_offset = f.offset;
            hi_sample = f.h.sample;
        } else {
            lo = f;
        }
    }

    frame_t next;
    bool at_end = false;
    while (target >= lo.h.sample + lo.h.block && next_frame(demux, &lo, &next, &at_end)) lo = next;

    demux->cursor = lo.offset;
    medf_buffer_seek(demux->reader, demux->cursor);
    if (landed_us) *landed_us = (int64_t)(lo.h.sample * 1000000ull / demux->info.sample_rate);
    return true;
}
