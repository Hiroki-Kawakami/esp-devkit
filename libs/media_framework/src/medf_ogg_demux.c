/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "medf_ogg.h"

#include <stdlib.h>
#include <string.h>

#include "esp_heap_caps.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

static const char *TAG = "medf_ogg_demux";

#define OGG_HEADER_BYTES 27
#define OGG_MAX_PAGE_BYTES (OGG_HEADER_BYTES + 255 + 255 * 255)
#define OGG_SCAN_BYTES 4096
#define OGG_SEEK_STEPS 48
#define OGG_MAX_COPIES 32
#define OGG_COPY_REF 0xC0000000u
#define OGG_MAX_PACKET_BYTES (256 * 1024)
#define OPUS_RATE 48000

enum {
    PAGE_CONTINUED = 1,
    PAGE_BOS = 2,
    PAGE_EOS = 4,
};

typedef struct {
    off_t offset;
    off_t body;
    uint32_t body_size;
    int64_t granule;
    uint32_t serial;
    uint8_t flags;
    uint8_t segments;
    uint8_t lacing[255];
} page_t;

typedef struct {
    page_t page;
    bool have_page;
    int seg;
    off_t seg_at;
    off_t next_page;
    int skip_packets;
    bool drop_continued;
    int64_t next_sample;
} cursor_t;

typedef struct {
    uint32_t ref;
    uint8_t *data;
} copy_t;

struct medf_ogg_demux {
    medf_buffer_t *reader;
    medf_ogg_info_t info;
    uint32_t serial;
    uint16_t pre_skip;
    off_t data_start;
    off_t data_end;
    int64_t start_sample;
    int64_t end_sample;
    cursor_t cur;
    SemaphoreHandle_t lock;
    copy_t copies[OGG_MAX_COPIES];
    uint32_t copy_count;
    uint32_t crc_table[256];
    uint8_t scan[OGG_SCAN_BYTES];
};

static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static int64_t le64(const uint8_t *p) {
    uint64_t v = 0;
    for (int i = 7; i >= 0; i--) v = v << 8 | p[i];
    return (int64_t)v;
}

static bool read_exact(medf_ogg_demux_t *demux, off_t at, void *out, size_t size) {
    medf_buffer_seek(demux->reader, at);
    return medf_buffer_read(demux->reader, out, size) == size;
}

static void crc_init(medf_ogg_demux_t *demux) {
    for (uint32_t i = 0; i < 256; i++) {
        uint32_t r = i << 24;
        for (int b = 0; b < 8; b++) r = r & 0x80000000u ? r << 1 ^ 0x04C11DB7u : r << 1;
        demux->crc_table[i] = r;
    }
}

static uint32_t crc_update(const medf_ogg_demux_t *demux, uint32_t crc, const uint8_t *p, size_t n) {
    while (n--) crc = crc << 8 ^ demux->crc_table[(crc >> 24) ^ *p++];
    return crc;
}

static bool load_page(medf_ogg_demux_t *demux, off_t at, page_t *page) {
    uint8_t h[OGG_HEADER_BYTES];
    if (at + OGG_HEADER_BYTES > demux->data_end || !read_exact(demux, at, h, sizeof(h))) return false;
    if (memcmp(h, "OggS", 4) != 0 || h[4] != 0) return false;
    page->offset = at;
    page->flags = h[5];
    page->granule = le64(h + 6);
    page->serial = le32(h + 14);
    page->segments = h[26];
    if (!read_exact(demux, at + OGG_HEADER_BYTES, page->lacing, page->segments)) return false;
    page->body = at + OGG_HEADER_BYTES + page->segments;
    page->body_size = 0;
    for (int i = 0; i < page->segments; i++) page->body_size += page->lacing[i];
    return page->body + (off_t)page->body_size <= demux->data_end;
}

static bool page_crc_ok(medf_ogg_demux_t *demux, const page_t *page) {
    const off_t end = page->body + (off_t)page->body_size;
    uint8_t h[OGG_HEADER_BYTES];
    if (!read_exact(demux, page->offset, h, sizeof(h))) return false;
    const uint32_t stored = le32(h + 22);
    memset(h + 22, 0, 4);
    uint32_t crc = crc_update(demux, 0, h, sizeof(h));
    crc = crc_update(demux, crc, page->lacing, page->segments);
    for (off_t at = page->body; at < end;) {
        const size_t n = end - at < (off_t)sizeof(demux->scan) ? (size_t)(end - at) : sizeof(demux->scan);
        if (!read_exact(demux, at, demux->scan, n)) return false;
        crc = crc_update(demux, crc, demux->scan, n);
        at += (off_t)n;
    }
    return crc == stored;
}

/* First page of this stream starting in [from, limit) that passes its CRC;
 * "OggS" turns up in compressed data too. */
static bool find_page(medf_ogg_demux_t *demux, off_t from, off_t limit, page_t *page) {
    if (limit > demux->data_end) limit = demux->data_end;
    off_t at = from;
    while (at < limit) {
        const size_t want = demux->data_end - at < (off_t)sizeof(demux->scan)
                                ? (size_t)(demux->data_end - at) : sizeof(demux->scan);
        if (want < 4 || !read_exact(demux, at, demux->scan, want)) return false;
        size_t usable = want - 3;
        if (at + (off_t)usable > limit) usable = (size_t)(limit - at);
        for (size_t i = 0; i < usable; i++) {
            if (demux->scan[i] != 'O' || memcmp(demux->scan + i, "OggS", 4) != 0) continue;
            const off_t candidate = at + (off_t)i;
            if (load_page(demux, candidate, page) && page->serial == demux->serial &&
                page_crc_ok(demux, page)) {
                return true;
            }
            if (!read_exact(demux, at, demux->scan, want)) return false;
        }
        at += (off_t)usable;
    }
    return false;
}

/* The next page of this stream from `at`; false at the end of the stream. */
static bool next_own_page(medf_ogg_demux_t *demux, off_t at, page_t *page) {
    while (at < demux->data_end) {
        if (!load_page(demux, at, page) && !find_page(demux, at + 1, demux->data_end, page)) {
            return false;
        }
        if (page->serial == demux->serial) return true;
        if (page->flags & PAGE_BOS && demux->data_start && page->offset >= demux->data_start) {
            return false;
        }
        at = page->body + (off_t)page->body_size;
    }
    return false;
}

static int completed_packets(const page_t *page) {
    int n = 0;
    for (int i = 0; i < page->segments; i++) n += page->lacing[i] < 255;
    return n;
}

static uint32_t opus_samples(const uint8_t *toc, size_t len) {
    if (!len) return 0;
    uint32_t frame;
    if (toc[0] & 0x80) {
        frame = 120u << ((toc[0] >> 3) & 3);
    } else if ((toc[0] & 0x60) == 0x60) {
        frame = toc[0] & 0x08 ? 960 : 480;
    } else {
        const int size = (toc[0] >> 3) & 3;
        frame = size == 3 ? 2880 : 480u << size;
    }
    switch (toc[0] & 3) {
    case 0: return frame;
    case 1:
    case 2: return 2 * frame;
    default: return len >= 2 ? frame * (toc[1] & 0x3F) : 0;
    }
}

static int64_t sample_to_us(const medf_ogg_demux_t *demux, int64_t sample) {
    return (sample - demux->start_sample - demux->pre_skip) * 1000000 / OPUS_RATE;
}

static bool enter_page(medf_ogg_demux_t *demux, cursor_t *c) {
    if (!next_own_page(demux, c->next_page, &c->page)) return false;
    c->have_page = true;
    c->seg = 0;
    c->seg_at = c->page.body;
    c->next_page = c->page.body + (off_t)c->page.body_size;
    if (c->drop_continued && (c->page.flags & PAGE_CONTINUED)) {
        while (c->seg < c->page.segments) {
            const uint8_t v = c->page.lacing[c->seg++];
            c->seg_at += v;
            if (v < 255) {
                if (c->skip_packets > 0) c->skip_packets--;
                break;
            }
        }
    }
    c->drop_continued = false;
    return true;
}

static bool more_completed(const cursor_t *c) {
    for (int i = c->seg; i < c->page.segments; i++) {
        if (c->page.lacing[i] < 255) return true;
    }
    return false;
}

static uint32_t add_copy(medf_ogg_demux_t *demux, uint8_t *data) {
    uint32_t ref = MEDF_NO_REF;
    xSemaphoreTake(demux->lock, portMAX_DELAY);
    for (uint32_t i = 0; i < OGG_MAX_COPIES; i++) {
        if (demux->copies[i].data) continue;
        demux->copy_count = (demux->copy_count + 1) & 0x3FFFFFFFu;
        demux->copies[i].ref = ref = OGG_COPY_REF | demux->copy_count;
        demux->copies[i].data = data;
        break;
    }
    xSemaphoreGive(demux->lock);
    return ref;
}

typedef enum {
    GATHER_OK,
    GATHER_DROPPED,
    GATHER_FAILED,
} gather_t;

/* A packet that continues onto later pages, gathered into one allocation.
 * One cut short by a page that does not continue it is dropped, and reading
 * goes on with that page. */
static gather_t gather(medf_ogg_demux_t *demux, cursor_t *c, uint8_t **out, uint32_t *size) {
    uint32_t len = 0;
    for (int s = c->seg; s < c->page.segments; s++) len += c->page.lacing[s];
    uint32_t cap = len * 2 > 4096 ? len * 2 : 4096;
    uint8_t *buf = heap_caps_malloc(cap, MALLOC_CAP_SPIRAM);
    if (!buf || !read_exact(demux, c->seg_at, buf, len)) {
        heap_caps_free(buf);
        return GATHER_FAILED;
    }
    for (;;) {
        if (!enter_page(demux, c)) {
            heap_caps_free(buf);
            return GATHER_FAILED;
        }
        if (!(c->page.flags & PAGE_CONTINUED)) {
            heap_caps_free(buf);
            ESP_LOGW(TAG, "dropped a packet cut short at %lld", (long long)c->page.offset);
            return GATHER_DROPPED;
        }
        uint32_t part = 0;
        bool done = false;
        while (c->seg < c->page.segments) {
            const uint8_t v = c->page.lacing[c->seg++];
            part += v;
            if (v < 255) {
                done = true;
                break;
            }
        }
        if (len + part > OGG_MAX_PACKET_BYTES) {
            heap_caps_free(buf);
            return GATHER_FAILED;
        }
        if (len + part > cap) {
            cap = (len + part) * 2;
            uint8_t *grown = heap_caps_realloc(buf, cap, MALLOC_CAP_SPIRAM);
            if (!grown) {
                heap_caps_free(buf);
                return GATHER_FAILED;
            }
            buf = grown;
        }
        if (!read_exact(demux, c->seg_at, buf + len, part)) {
            heap_caps_free(buf);
            return GATHER_FAILED;
        }
        c->seg_at += part;
        len += part;
        if (done) break;
    }
    *out = buf;
    *size = len;
    return GATHER_OK;
}

/* Steps over a packet that continues onto later pages, keeping its first
 * bytes. */
static gather_t skip_spanning(medf_ogg_demux_t *demux, cursor_t *c, uint8_t *toc) {
    if (!read_exact(demux, c->seg_at, toc, 2)) return GATHER_FAILED;
    for (;;) {
        if (!enter_page(demux, c)) return GATHER_FAILED;
        if (!(c->page.flags & PAGE_CONTINUED)) return GATHER_DROPPED;
        while (c->seg < c->page.segments) {
            const uint8_t v = c->page.lacing[c->seg++];
            c->seg_at += v;
            if (v < 255) return GATHER_OK;
        }
    }
}

/* The next packet at `c`. With `packet` NULL only its timing is worked out,
 * from its first two bytes. */
static bool next_packet(medf_ogg_demux_t *demux, cursor_t *c, medf_ogg_packet_t *packet,
                        int64_t *start, uint32_t *samples) {
    for (;;) {
        if (!c->have_page && !enter_page(demux, c)) return false;
        if (c->seg >= c->page.segments) {
            c->have_page = false;
            continue;
        }
        const off_t at = c->seg_at;
        uint32_t len = 0;
        bool complete = false;
        int s = c->seg;
        while (s < c->page.segments) {
            const uint8_t v = c->page.lacing[s++];
            len += v;
            if (v < 255) {
                complete = true;
                break;
            }
        }
        uint8_t toc[2] = { 0, 0 };
        const uint8_t *data = NULL;
        uint8_t *copy = NULL;
        uint32_t ref = MEDF_NO_REF;
        if (complete) {
            c->seg = s;
            c->seg_at = at + len;
            if (c->skip_packets > 0) {
                c->skip_packets--;
                if (!more_completed(c) && c->page.granule >= 0) c->next_sample = c->page.granule;
                continue;
            }
            if (!len) continue;
            if (packet) {
                data = medf_buffer_view_at(demux->reader, at, len, &ref);
                if (!data) return false;
            } else if (!read_exact(demux, at, toc, len < 2 ? 1 : 2)) {
                return false;
            }
        } else {
            const gather_t got = packet ? gather(demux, c, &copy, &len) : skip_spanning(demux, c, toc);
            if (got == GATHER_FAILED) return false;
            if (got == GATHER_DROPPED) continue;
            data = copy;
        }
        const uint32_t n = data ? opus_samples(data, len) : opus_samples(toc, len < 2 ? len : 2);
        *start = c->next_sample;
        *samples = n;
        c->next_sample += n;
        if (!more_completed(c) && c->page.granule >= 0) c->next_sample = c->page.granule;
        if (packet) {
            if (copy) {
                ref = add_copy(demux, copy);
                if (ref == MEDF_NO_REF) {
                    heap_caps_free(copy);
                    ESP_LOGE(TAG, "too many packets spanning pages in flight");
                    return false;
                }
            }
            packet->pts_us = sample_to_us(demux, *start);
            packet->data = data;
            packet->size = len;
            packet->ref = ref;
        }
        return true;
    }
}

typedef struct {
    medf_ogg_demux_t *demux;
    cursor_t *c;
    uint32_t seg_left;
} tag_stream_t;

static bool tag_advance(tag_stream_t *t) {
    cursor_t *c = t->c;
    while (!t->seg_left) {
        if (c->seg >= c->page.segments && !enter_page(t->demux, c)) return false;
        if (c->seg >= c->page.segments) continue;
        t->seg_left = c->page.lacing[c->seg++];
    }
    return true;
}

static bool tag_read(void *ctx, void *out, size_t size) {
    tag_stream_t *t = ctx;
    uint8_t *p = out;
    while (size) {
        if (!tag_advance(t)) return false;
        const size_t n = size < t->seg_left ? size : t->seg_left;
        if (p) {
            if (!read_exact(t->demux, t->c->seg_at, p, n)) return false;
            p += n;
        }
        t->c->seg_at += (off_t)n;
        t->seg_left -= (uint32_t)n;
        size -= n;
    }
    return true;
}

static bool tag_skip(void *ctx, size_t size) {
    return tag_read(ctx, NULL, size);
}

/* The OpusTags packet, read where it lies: it can span pages. Audio starts
 * on the page after the one it ends on, returned in *data_start. */
static bool read_opus_tags(medf_ogg_demux_t *demux, cursor_t *c, bool want_cover, off_t *data_start) {
    cursor_t probe = *c;
    uint32_t len = 0;
    for (bool done = false; !done;) {
        if (!enter_page(demux, &probe)) return false;
        for (int i = 0; i < probe.page.segments; i++) {
            len += probe.page.lacing[i];
            if (probe.page.lacing[i] < 255) {
                done = true;
                break;
            }
        }
    }
    *data_start = probe.next_page;
    if (!enter_page(demux, c)) return false;
    tag_stream_t t = { demux, c, 0 };
    uint8_t magic[8];
    if (len < 8 || !tag_read(&t, magic, sizeof(magic)) || memcmp(magic, "OpusTags", 8) != 0) {
        return false;
    }
    const medf_stream_t in = { &t, tag_read, tag_skip };
    medf_tags_read_vorbis_comment_stream(&in, len - 8, &demux->info.tags, want_cover);
    demux->info.tags.cover_scanned = want_cover;
    return true;
}

static int64_t find_end_sample(medf_ogg_demux_t *demux) {
    for (off_t window = OGG_MAX_PAGE_BYTES;; window *= 2) {
        off_t from = demux->data_end - window;
        if (from < demux->data_start) from = demux->data_start;
        int64_t last = -1;
        page_t page;
        for (off_t at = from; find_page(demux, at, demux->data_end, &page);) {
            if (page.granule >= 0) last = page.granule;
            at = page.body + (off_t)page.body_size;
        }
        if (last >= 0 || from == demux->data_start) return last;
    }
}

medf_ogg_demux_t *medf_ogg_demux_open(const char *path, const medf_arena_t *arena,
                                      bool want_cover, const char **error) {
    const char *ignored = NULL;
    if (!error) error = &ignored;
    *error = NULL;

    medf_ogg_demux_t *demux = heap_caps_calloc(1, sizeof(*demux), MALLOC_CAP_SPIRAM);
    if (!demux) {
        *error = "out of memory";
        return NULL;
    }
    demux->lock = xSemaphoreCreateMutex();
    demux->reader = demux->lock ? medf_buffer_open(path, arena) : NULL;
    if (!demux->reader) {
        *error = "cannot open the file";
        medf_ogg_demux_close(demux);
        return NULL;
    }
    crc_init(demux);
    demux->data_end = medf_buffer_size(demux->reader);

    page_t first;
    uint8_t head[MEDF_OGG_OPUS_HEAD_BYTES];
    if (!load_page(demux, 0, &first) || !(first.flags & PAGE_BOS) || !first.segments) {
        *error = "not an Ogg file";
        medf_ogg_demux_close(demux);
        return NULL;
    }
    demux->serial = first.serial;
    if (first.body_size < sizeof(head) || !read_exact(demux, first.body, head, sizeof(head)) ||
        memcmp(head, "OpusHead", 8) != 0) {
        *error = "only Opus is supported in Ogg";
        medf_ogg_demux_close(demux);
        return NULL;
    }
    memcpy(demux->info.opus_head, head, sizeof(head));
    demux->info.channels = head[9];
    demux->info.sample_rate = OPUS_RATE;
    demux->pre_skip = (uint16_t)(head[10] | head[11] << 8);

    cursor_t *c = &demux->cur;
    c->next_page = first.body + (off_t)first.body_size;
    off_t data_start = 0;
    if (!read_opus_tags(demux, c, want_cover, &data_start)) {
        *error = "no OpusTags in this Ogg file";
        medf_ogg_demux_close(demux);
        return NULL;
    }
    demux->data_start = data_start;

    cursor_t probe = { .next_page = demux->data_start };
    if (!enter_page(demux, &probe)) {
        *error = "no Opus audio in this Ogg file";
        medf_ogg_demux_close(demux);
        return NULL;
    }
    int64_t start = probe.page.granule;
    if (start >= 0) {
        cursor_t walk = probe;
        for (int n = completed_packets(&probe.page); n > 0; n--) {
            int64_t at;
            uint32_t samples;
            if (!next_packet(demux, &walk, NULL, &at, &samples)) break;
            start -= samples;
        }
    }
    demux->start_sample = start > 0 ? start : 0;
    demux->end_sample = find_end_sample(demux);

    const int64_t total = demux->end_sample - demux->start_sample - demux->pre_skip;
    demux->info.duration_us = total > 0 ? total * 1000000 / OPUS_RATE : 0;
    demux->info.bitrate_bps =
        demux->info.duration_us > 0
            ? (uint32_t)((uint64_t)(demux->data_end - demux->data_start) * 8 * 1000000ull /
                         (uint64_t)demux->info.duration_us)
            : 0;

    ESP_LOGI(TAG, "%s: Opus x%u, pre-skip %u, %lld us", path, (unsigned)demux->info.channels,
             (unsigned)demux->pre_skip, (long long)demux->info.duration_us);

    *c = (cursor_t){ .next_page = demux->data_start, .next_sample = demux->start_sample };
    medf_buffer_seek(demux->reader, demux->data_start);
    medf_buffer_set_readahead(demux->reader, true);
    return demux;
}

void medf_ogg_demux_close(medf_ogg_demux_t *demux) {
    if (!demux) return;
    if (demux->lock) medf_ogg_demux_release_all(demux);
    medf_tags_free(&demux->info.tags);
    if (demux->reader) medf_buffer_close(demux->reader);
    if (demux->lock) vSemaphoreDelete(demux->lock);
    heap_caps_free(demux);
}

const medf_ogg_info_t *medf_ogg_demux_info(const medf_ogg_demux_t *demux) {
    return demux ? &demux->info : NULL;
}

medf_buffer_t *medf_ogg_demux_buffer(medf_ogg_demux_t *demux) {
    return demux ? demux->reader : NULL;
}

bool medf_ogg_demux_read(medf_ogg_demux_t *demux, medf_ogg_packet_t *packet) {
    if (!demux || !packet) return false;
    cursor_t c = demux->cur;
    int64_t start;
    uint32_t samples;
    if (!next_packet(demux, &c, packet, &start, &samples)) return false;
    demux->cur = c;
    return true;
}

void medf_ogg_demux_release(medf_ogg_demux_t *demux, uint32_t ref) {
    if (!demux || ref == MEDF_NO_REF) return;
    uint8_t *data = NULL;
    xSemaphoreTake(demux->lock, portMAX_DELAY);
    for (uint32_t i = 0; i < OGG_MAX_COPIES; i++) {
        if (demux->copies[i].data && demux->copies[i].ref == ref) {
            data = demux->copies[i].data;
            demux->copies[i].data = NULL;
            break;
        }
    }
    xSemaphoreGive(demux->lock);
    if (data) {
        heap_caps_free(data);
    } else {
        medf_buffer_release(demux->reader, ref);
    }
}

void medf_ogg_demux_release_all(medf_ogg_demux_t *demux) {
    if (!demux) return;
    xSemaphoreTake(demux->lock, portMAX_DELAY);
    for (uint32_t i = 0; i < OGG_MAX_COPIES; i++) {
        heap_caps_free(demux->copies[i].data);
        demux->copies[i].data = NULL;
    }
    xSemaphoreGive(demux->lock);
    if (demux->reader) medf_buffer_release_all(demux->reader);
}

bool medf_ogg_demux_seek(medf_ogg_demux_t *demux, int64_t pts_us, int64_t *landed_us) {
    if (!demux) return false;
    const int64_t target = demux->start_sample + demux->pre_skip +
                           (pts_us > 0 ? pts_us * OPUS_RATE / 1000000 : 0);

    page_t lo = { .offset = -1 };
    off_t lo_offset = demux->data_start;
    off_t hi_offset = demux->data_end;
    for (int step = 0; step < OGG_SEEK_STEPS && hi_offset - lo_offset > OGG_MAX_PAGE_BYTES; step++) {
        const off_t mid = lo_offset + (hi_offset - lo_offset) / 2;
        page_t page;
        if (!find_page(demux, mid, hi_offset, &page)) {
            hi_offset = mid;
            continue;
        }
        if (page.granule >= 0 && page.granule <= target) {
            lo = page;
            lo_offset = page.offset;
        } else {
            hi_offset = page.offset;
        }
    }
    page_t page;
    for (off_t at = lo_offset; at < demux->data_end && next_own_page(demux, at, &page);) {
        if (page.granule > target) break;
        if (page.granule >= 0) lo = page;
        at = page.body + (off_t)page.body_size;
    }

    cursor_t c = { .next_page = demux->data_start, .next_sample = demux->start_sample };
    if (lo.offset >= 0) {
        c.next_page = lo.offset;
        c.skip_packets = completed_packets(&lo);
        c.drop_continued = true;
        c.next_sample = lo.granule;
    }
    for (;;) {
        cursor_t probe = c;
        int64_t start;
        uint32_t samples;
        if (!next_packet(demux, &probe, NULL, &start, &samples)) break;
        if (start + samples > target) break;
        c = probe;
    }
    demux->cur = c;
    medf_buffer_seek(demux->reader, c.have_page ? c.seg_at : c.next_page);
    if (landed_us) {
        const int64_t landed = sample_to_us(demux, c.next_sample);
        *landed_us = landed > 0 ? landed : 0;
    }
    return true;
}
