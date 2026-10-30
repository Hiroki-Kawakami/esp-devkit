/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Writes what media_framework makes of a file as text. src/tests/differential.rs
 * writes the same text from devkit_media and compares the two.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "medf_avi.h"
#include "medf_es.h"
#include "medf_flac.h"
#include "medf_mkv.h"
#include "medf_mp4.h"
#include "medf_ogg.h"
#include "medf_wav.h"

#define ARENA_BYTES (4 * 1024 * 1024)
#define SMALL_ARENA_BYTES (1024 * 1024)
#define HELD_PACKETS 4
#define SEEK_READS 6

static FILE *out;

static uint64_t fnv(const void *data, size_t size) {
    const uint8_t *p = data;
    uint64_t hash = 0xCBF29CE484222325ull;
    while (size--) hash = (hash ^ *p++) * 0x100000001B3ull;
    return hash;
}

typedef struct {
    int kind;
    int64_t pts;
    bool keyframe;
    const uint8_t *data;
    uint32_t size;
    uint32_t ref;
} packet_t;

typedef struct {
    void *demux;
    medf_buffer_t *buffer;
    bool (*read)(void *demux, packet_t *packet, bool want_audio);
    void (*release)(void *demux, uint32_t ref);
    bool (*seek)(void *demux, int64_t target, int64_t *landed);
    void (*close)(void *demux);
    const medf_tags_t *tags;
    int64_t duration;
    int64_t frame_count;
    bool has_video;
    bool has_audio;
    bool video_keys;
} source_t;

static const char *video_names[] = { "none", "MJPEG", "H264", "MPEG2", "unsupported" };
static const char *riff_names[] = { "none", "PCM", "MP3", "MP2", "IMA", "AAC", "unsupported" };
static const char *mkv_names[] = { "none", "PCM", "MP3", "MP2", "IMA", "AAC", "Opus", "FLAC", "ALAC", "unsupported" };
static const char *mp4_names[] = { "none", "PCM", "MP3", "AAC", "Opus", "FLAC", "ALAC", "unsupported" };

static void print_video(const char *codec, uint32_t w, uint32_t h, int64_t interval, int rotation,
                        uint32_t count, uint32_t max, const uint8_t *priv, uint32_t priv_size) {
    fprintf(out, "video %s %u %u %lld %d %u %u %u:%016llx\n", codec, w, h, (long long)interval, rotation,
            count, max, priv_size, (unsigned long long)(priv ? fnv(priv, priv_size) : fnv("", 0)));
}

static void print_audio(const char *codec, uint32_t rate, uint32_t bitrate, int channels, int bits,
                        int align, uint32_t max, const uint8_t *priv, uint32_t priv_size) {
    fprintf(out, "audio %s %u %u %d %d %d %u %u:%016llx\n", codec, rate, bitrate, channels, bits, align,
            max, priv_size, (unsigned long long)(priv ? fnv(priv, priv_size) : fnv("", 0)));
}

static void print_tags(const medf_tags_t *t) {
    fprintf(out, "tags [%s] [%s] [%s] [%s] [%s] [%s]\n", t->title, t->artist, t->album, t->album_artist,
            t->track, t->date);
    fprintf(out, "cover %u %d %d %016llx at %lld %u scanned %d\n", (unsigned)t->cover.size,
            (int)t->cover.format, (int)t->cover.front,
            (unsigned long long)(t->cover.data ? fnv(t->cover.data, t->cover.size) : fnv("", 0)),
            (long long)t->cover_at.offset, (unsigned)t->cover_at.size, (int)t->cover_scanned);
}

/* AVI: the target is a frame number and pts is the frame number. */
static bool avi_read(void *d, packet_t *p, bool want_audio) {
    medf_avi_packet_t a;
    if (!medf_avi_demux_read(d, &a, want_audio)) return false;
    *p = (packet_t){ a.type == MEDF_AVI_PACKET_VIDEO ? 'V' : 'A', a.frame_index, a.keyframe, a.data, a.size, a.ref };
    return true;
}
static bool avi_seek(void *d, int64_t target, int64_t *landed) {
    uint32_t frame = 0;
    if (!medf_avi_demux_seek(d, (uint32_t)target, &frame)) return false;
    *landed = frame;
    return true;
}
static void avi_close(void *d) { medf_avi_demux_close(d); }
static void avi_release(void *d, uint32_t ref) { medf_buffer_release(medf_avi_demux_buffer(d), ref); }

static bool mkv_read(void *d, packet_t *p, bool want_audio) {
    medf_mkv_packet_t a;
    if (!medf_mkv_demux_read(d, &a, want_audio)) return false;
    *p = (packet_t){ a.type == MEDF_MKV_PACKET_VIDEO ? 'V' : 'A', a.pts_us, a.keyframe, a.data, a.size, a.ref };
    return true;
}
static bool mkv_seek(void *d, int64_t t, int64_t *l) { return medf_mkv_demux_seek(d, t, l); }
static void mkv_close(void *d) { medf_mkv_demux_close(d); }
static void mkv_release(void *d, uint32_t ref) { medf_buffer_release(medf_mkv_demux_buffer(d), ref); }

static bool mp4_read(void *d, packet_t *p, bool want_audio) {
    medf_mp4_packet_t a;
    if (!medf_mp4_demux_read(d, &a, want_audio)) return false;
    *p = (packet_t){ a.type == MEDF_MP4_PACKET_VIDEO ? 'V' : 'A', a.pts_us, a.keyframe, a.data, a.size, a.ref };
    return true;
}
static bool mp4_seek(void *d, int64_t t, int64_t *l) { return medf_mp4_demux_seek(d, t, l); }
static void mp4_close(void *d) { medf_mp4_demux_close(d); }
static void mp4_release(void *d, uint32_t ref) { medf_buffer_release(medf_mp4_demux_buffer(d), ref); }

#define AUDIO_ONLY(name, packet_type)                                               \
    static bool name##_read(void *d, packet_t *p, bool want_audio) {                \
        (void)want_audio;                                                           \
        packet_type a;                                                              \
        if (!medf_##name##_demux_read(d, &a)) return false;                         \
        *p = (packet_t){ 'A', a.pts_us, true, a.data, a.size, a.ref };              \
        return true;                                                                \
    }                                                                               \
    static bool name##_seek(void *d, int64_t t, int64_t *l) { return medf_##name##_demux_seek(d, t, l); } \
    static void name##_close(void *d) { medf_##name##_demux_close(d); }

AUDIO_ONLY(wav, medf_wav_packet_t)
AUDIO_ONLY(es, medf_es_packet_t)
AUDIO_ONLY(flac, medf_flac_packet_t)
AUDIO_ONLY(ogg, medf_ogg_packet_t)

static void wav_release(void *d, uint32_t ref) { medf_buffer_release(medf_wav_demux_buffer(d), ref); }
static void es_release(void *d, uint32_t ref) { medf_buffer_release(medf_es_demux_buffer(d), ref); }
static void flac_release(void *d, uint32_t ref) { medf_buffer_release(medf_flac_demux_buffer(d), ref); }
static void ogg_release(void *d, uint32_t ref) { medf_ogg_demux_release(d, ref); }

static const char *extension(const char *path) {
    const char *dot = strrchr(path, '.');
    return dot ? dot + 1 : "";
}

static bool open_source(const char *path, const medf_arena_t *arena, bool cover, source_t *s, const char **error) {
    memset(s, 0, sizeof(*s));
    const char *ext = extension(path);
    if (!strcmp(ext, "avi")) {
        medf_avi_demux_t *d = medf_avi_demux_open(path, arena, cover, error);
        if (!d) return false;
        const medf_avi_info_t *i = medf_avi_demux_info(d);
        *s = (source_t){ d, medf_avi_demux_buffer(d), avi_read, avi_release, avi_seek, avi_close, &i->tags,
                         (int64_t)i->video.frame_count * i->video.frame_interval_us, i->video.frame_count,
                         true, i->audio.codec != MEDF_RIFF_AUDIO_CODEC_NONE, true };
        fprintf(out, "container avi\n");
        print_video(video_names[i->video.codec], i->video.width, i->video.height, i->video.frame_interval_us, 0,
                    i->video.frame_count, i->video.max_frame_bytes, i->video.codec_private, i->video.codec_private_size);
        if (s->has_audio) {
            print_audio(riff_names[i->audio.codec], i->audio.sample_rate, i->audio.bitrate_bps, i->audio.channels,
                        i->audio.bits_per_sample, i->audio.block_align, i->audio.max_frame_bytes,
                        i->audio.codec_private, i->audio.codec_private_size);
        }
        fprintf(out, "time %lld 1 %d\n", (long long)s->duration, (int)i->seekable);
    } else if (!strcmp(ext, "wav")) {
        medf_wav_demux_t *d = medf_wav_demux_open(path, arena, cover, error);
        if (!d) return false;
        const medf_wav_info_t *i = medf_wav_demux_info(d);
        *s = (source_t){ d, medf_wav_demux_buffer(d), wav_read, wav_release, wav_seek, wav_close, &i->tags,
                         i->duration_us, 0, false, true, false };
        fprintf(out, "container wav\n");
        print_audio(riff_names[i->codec], i->sample_rate, i->bitrate_bps, i->channels, i->bits_per_sample,
                    i->block_align, 0, i->codec_private, i->codec_private_size);
        fprintf(out, "time %lld 1 1\n", (long long)i->duration_us);
    } else if (!strcmp(ext, "mp3") || !strcmp(ext, "mp2") || !strcmp(ext, "aac")) {
        medf_es_demux_t *d = medf_es_demux_open(path, arena, cover, error);
        if (!d) return false;
        const medf_es_info_t *i = medf_es_demux_info(d);
        static const char *names[] = { "none", "MP3", "MP2", "AAC" };
        *s = (source_t){ d, medf_es_demux_buffer(d), es_read, es_release, es_seek, es_close, &i->tags,
                         i->duration_us, 0, false, true, false };
        fprintf(out, "container es\n");
        print_audio(names[i->codec], i->sample_rate, i->bitrate_bps, i->channels, 0, 0, 0, NULL, 0);
        fprintf(out, "time %lld %d 1\n", (long long)i->duration_us, (int)i->duration_exact);
    } else if (!strcmp(ext, "flac")) {
        medf_flac_demux_t *d = medf_flac_demux_open(path, arena, cover, error);
        if (!d) return false;
        const medf_flac_info_t *i = medf_flac_demux_info(d);
        *s = (source_t){ d, medf_flac_demux_buffer(d), flac_read, flac_release, flac_seek, flac_close, &i->tags,
                         i->duration_us, 0, false, true, false };
        fprintf(out, "container flac\n");
        print_audio("FLAC", i->sample_rate, i->bitrate_bps, i->channels, i->bits_per_sample, 0, 0, i->streaminfo,
                    MEDF_FLAC_STREAMINFO_BYTES);
        fprintf(out, "time %lld 1 1\n", (long long)i->duration_us);
    } else if (!strcmp(ext, "opus")) {
        medf_ogg_demux_t *d = medf_ogg_demux_open(path, arena, cover, error);
        if (!d) return false;
        const medf_ogg_info_t *i = medf_ogg_demux_info(d);
        *s = (source_t){ d, medf_ogg_demux_buffer(d), ogg_read, ogg_release, ogg_seek, ogg_close, &i->tags,
                         i->duration_us, 0, false, true, false };
        fprintf(out, "container ogg\n");
        print_audio("Opus", i->sample_rate, i->bitrate_bps, i->channels, 0, 0, 0, i->opus_head, MEDF_OGG_OPUS_HEAD_BYTES);
        fprintf(out, "time %lld 1 1\n", (long long)i->duration_us);
    } else if (!strcmp(ext, "mkv") || !strcmp(ext, "webm")) {
        medf_mkv_demux_t *d = medf_mkv_demux_open(path, arena, cover, error);
        if (!d) return false;
        const medf_mkv_info_t *i = medf_mkv_demux_info(d);
        *s = (source_t){ d, medf_mkv_demux_buffer(d), mkv_read, mkv_release, mkv_seek, mkv_close, &i->tags,
                         i->duration_us, 0, true, i->audio.codec != MEDF_MKV_AUDIO_CODEC_NONE, true };
        fprintf(out, "container mkv\n");
        print_video(video_names[i->video.codec], i->video.width, i->video.height, i->video.frame_interval_us,
                    i->video.rotation_ccw, 0, 0, i->video.codec_private, i->video.codec_private_size);
        if (s->has_audio) {
            print_audio(mkv_names[i->audio.codec], i->audio.sample_rate, 0, i->audio.channels,
                        i->audio.bits_per_sample, i->audio.block_align, 0, i->audio.codec_private,
                        i->audio.codec_private_size);
        }
        fprintf(out, "time %lld 1 %d\n", (long long)i->duration_us, (int)i->seekable);
    } else {
        medf_mp4_demux_t *d = medf_mp4_demux_open(path, arena, cover, error);
        if (!d) return false;
        const medf_mp4_info_t *i = medf_mp4_demux_info(d);
        s->has_video = i->video.codec != MEDF_MP4_VIDEO_CODEC_NONE;
        s->has_audio = i->audio.codec != MEDF_MP4_AUDIO_CODEC_NONE;
        const bool has_video = s->has_video, has_audio = s->has_audio;
        *s = (source_t){ d, medf_mp4_demux_buffer(d), mp4_read, mp4_release, mp4_seek, mp4_close, &i->tags,
                         i->duration_us, 0, has_video, has_audio, has_video };
        fprintf(out, "container mp4\n");
        if (has_video) {
            print_video(video_names[i->video.codec], i->video.width, i->video.height, i->video.frame_interval_us,
                        i->video.rotation_ccw, 0, 0, i->video.codec_private, i->video.codec_private_size);
        }
        if (has_audio) {
            print_audio(mp4_names[i->audio.codec], i->audio.sample_rate, i->audio.bitrate_bps, i->audio.channels,
                        i->audio.bits_per_sample, 0, 0, i->audio.codec_private, i->audio.codec_private_size);
        }
        fprintf(out, "time %lld 1 %d\n", (long long)i->duration_us, (int)i->seekable);
    }
    return true;
}

static void print_packet(const packet_t *p) {
    fprintf(out, "%c %lld %d %u %016llx\n", p->kind, (long long)p->pts, (int)p->keyframe, (unsigned)p->size,
            (unsigned long long)fnv(p->data, p->size));
}

static void read_all(source_t *s, bool want_audio, int hold) {
    uint32_t held[HELD_PACKETS] = { 0 };
    int count = 0;
    packet_t p;
    long packets = 0;
    while (s->read(s->demux, &p, want_audio)) {
        print_packet(&p);
        packets++;
        if (hold) {
            if (count == hold) {
                s->release(s->demux, held[0]);
                memmove(held, held + 1, sizeof(held[0]) * (size_t)(hold - 1));
                count--;
            }
            held[count++] = p.ref;
        } else {
            s->release(s->demux, p.ref);
        }
    }
    for (int i = 0; i < count; i++) s->release(s->demux, held[i]);
    fprintf(out, "end %ld\n", packets);
}

static void seek_to(source_t *s, int64_t target, bool want_audio) {
    int64_t landed = 0;
    const bool ok = s->seek(s->demux, target, &landed);
    fprintf(out, "seek %lld %d %lld\n", (long long)target, (int)ok, ok ? (long long)landed : 0);
    if (!ok) return;
    packet_t p;
    for (int i = 0; i < SEEK_READS && s->read(s->demux, &p, want_audio); i++) {
        print_packet(&p);
        s->release(s->demux, p.ref);
    }
}

static void seeks(source_t *s, bool frames, bool want_audio) {
    const int64_t span = frames ? s->frame_count : s->duration;
    seek_to(s, -1, want_audio);
    seek_to(s, 0, want_audio);
    seek_to(s, 1, want_audio);
    for (int k = 1; k <= 7; k++) seek_to(s, span * k / 8, want_audio);
    seek_to(s, span - 1, want_audio);
    seek_to(s, span, want_audio);
    seek_to(s, span + (frames ? 1000 : 1000000), want_audio);
    seek_to(s, span / 3, want_audio);
    seek_to(s, 0, want_audio);
}

static void keyframes(source_t *s, const char *path) {
    const bool frames = !strcmp(extension(path), "avi");
    const int64_t span = frames ? s->frame_count : s->duration;
    for (int k = 0; k <= 8; k++) {
        const int64_t target = span * k / 8;
        const char *ext = extension(path);
        bool have_key = false, have_at = false;
        int64_t key = 0;
        uint64_t offset = 0;
        uint32_t size = 0;
        if (!strcmp(ext, "avi")) {
            uint32_t frame = 0;
            have_key = medf_avi_demux_keyframe_before(s->demux, (uint32_t)target, &frame);
            key = frame;
            have_at = medf_avi_demux_keyframe_at(s->demux, (uint32_t)target, &offset, &size);
        } else if (!strcmp(ext, "mkv") || !strcmp(ext, "webm")) {
            have_key = medf_mkv_demux_keyframe_before(s->demux, target, &key);
            have_at = medf_mkv_demux_keyframe_at(s->demux, target, &offset, &size);
        } else {
            have_key = medf_mp4_demux_keyframe_before(s->demux, target, &key);
            have_at = medf_mp4_demux_keyframe_at(s->demux, target, &offset, &size);
        }
        fprintf(out, "key %lld %d %lld %d %llu %u\n", (long long)target, (int)have_key, have_key ? (long long)key : 0,
                (int)have_at, have_at ? (unsigned long long)offset : 0, have_at ? (unsigned)size : 0);
    }
    s->seek(s->demux, 0, &(int64_t){ 0 });
}

static uint8_t *arena_bytes(size_t size) {
    void *data = NULL;
    if (posix_memalign(&data, MEDF_ARENA_ALIGNMENT, size)) abort();
    return data;
}

static void scenario(const char *path, size_t bytes, bool direct, bool cover, bool full, int hold, const char *name) {
    medf_arena_t arena = { arena_bytes(bytes), bytes, direct };
    const char *error = NULL;
    source_t s;
    fprintf(out, "== %s\n", name);
    if (!open_source(path, &arena, cover, &s, &error)) {
        fprintf(out, "failed [%s]\n", error ? error : "");
    } else {
        print_tags(s.tags);
        if (direct) {
            packet_t p;
            const bool got = s.read(s.demux, &p, true);
            fprintf(out, "direct read %d\n", (int)got);
            if (got) s.release(s.demux, p.ref);
        } else {
            read_all(&s, true, hold);
            if (full) {
                const bool frames = !strcmp(extension(path), "avi");
                seeks(&s, frames, true);
                if (s.video_keys) keyframes(&s, path);
                if (s.has_video && s.has_audio) {
                    read_all(&s, false, 0);
                    seeks(&s, frames, false);
                }
            }
        }
        s.close(s.demux);
    }
    free(arena.data);
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    out = fopen(argv[2], "w");
    if (!out) return 2;
    scenario(argv[1], ARENA_BYTES, false, true, true, 0, "full");
    scenario(argv[1], SMALL_ARENA_BYTES, false, true, false, HELD_PACKETS, "small");
    scenario(argv[1], ARENA_BYTES, false, false, false, 0, "nocover");
    scenario(argv[1], ARENA_BYTES, true, false, false, 0, "direct");
    fclose(out);
    return 0;
}
