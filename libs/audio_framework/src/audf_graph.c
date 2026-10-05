/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_graph.h"
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include "audf_alloc.h"
#include "audf_convert.h"
#include "audf_internal.h"

#define DEFAULT_MAX_FRAMES 512

typedef enum {
    NODE_INPUT,
    NODE_SOURCE,
    NODE_FIFO,
    NODE_EQ,
    NODE_GAIN,
    NODE_CONVERT,
    NODE_MIXER,
    NODE_RESAMPLER,
    NODE_CUSTOM,
    NODE_SINK,
} node_kind_t;

struct audf_node {
    node_kind_t  kind;
    audf_fmt_t   fmt;
    uint8_t      channels;
    audf_node_t **srcs;
    size_t       num_srcs;
    audf_node_t **consumers;
    size_t       num_consumers;
    bool         push;

    void        *buf;
    size_t       cap;
    const void **ins;

    union {
        audf_eq_t        *eq;
        audf_gain_t      *gain;
        audf_mixer_t     *mixer;
        audf_fifo_t      *fifo;
        audf_resampler_t *resampler;
    };
    audf_drift_t   *drift;
    audf_fifo_t    *drift_fifo;
    uint32_t        drift_generation;
    size_t          drift_elapsed;
    audf_source_read_t source_read;
    audf_sink_write_t  sink_write;
    const audf_node_ops_t *ops;
    void           *user;

    audf_node_t *next;
};

struct audf_graph {
    size_t       max_frames;
    uint32_t     caps;
    audf_node_t *head;
    audf_node_t *tail;
    size_t       count;
    esp_err_t    err;
    bool         built;
};

esp_err_t audf_graph_create(const audf_graph_config_t *config, audf_graph_t **out) {
    if (!out) return ESP_ERR_INVALID_ARG;
    const uint32_t caps = config ? config->alloc_caps : 0;
    audf_graph_t *graph = audf_calloc(1, sizeof(*graph), caps);
    if (!graph) return ESP_ERR_NO_MEM;
    graph->caps = caps;
    graph->max_frames = config && config->max_frames ? config->max_frames : DEFAULT_MAX_FRAMES;
    *out = graph;
    return ESP_OK;
}

void audf_graph_destroy(audf_graph_t *graph) {
    if (!graph) return;
    audf_node_t *node = graph->head;
    while (node) {
        audf_node_t *next = node->next;
        audf_free(node->srcs);
        audf_free(node->consumers);
        audf_free(node->buf);
        audf_free(node->ins);
        audf_free(node);
        node = next;
    }
    audf_free(graph);
}

static audf_node_t *fail(audf_graph_t *graph, esp_err_t err) {
    if (graph && graph->err == ESP_OK) graph->err = err;
    return NULL;
}

static audf_node_t *add(audf_graph_t *graph, node_kind_t kind, audf_node_t *const srcs[], size_t num_srcs,
                        audf_fmt_t fmt, uint8_t channels) {
    if (!graph) return NULL;
    if (graph->built) return fail(graph, ESP_ERR_INVALID_STATE);
    if (graph->err != ESP_OK) return NULL;
    if (channels < 1 || channels > AUDF_MAX_CHANNELS) return fail(graph, ESP_ERR_INVALID_ARG);
    for (size_t i = 0; i < num_srcs; i++) {
        if (!srcs[i]) return fail(graph, ESP_ERR_INVALID_ARG);
        if (srcs[i]->kind == NODE_SINK) return fail(graph, ESP_ERR_INVALID_ARG);
    }
    audf_node_t *node = audf_calloc(1, sizeof(*node), graph->caps);
    if (!node) return fail(graph, ESP_ERR_NO_MEM);
    if (num_srcs) {
        node->srcs = audf_malloc(num_srcs * sizeof(audf_node_t *), graph->caps);
        node->ins = audf_calloc(num_srcs, sizeof(void *), graph->caps);
        if (!node->srcs || !node->ins) {
            audf_free(node->srcs);
            audf_free(node->ins);
            audf_free(node);
            return fail(graph, ESP_ERR_NO_MEM);
        }
        memcpy(node->srcs, srcs, num_srcs * sizeof(audf_node_t *));
    }
    node->kind = kind;
    node->num_srcs = num_srcs;
    node->fmt = fmt;
    node->channels = channels;
    if (graph->tail) {
        graph->tail->next = node;
    } else {
        graph->head = node;
    }
    graph->tail = node;
    graph->count++;
    return node;
}

static bool matches(const audf_node_t *src, audf_fmt_t fmt, uint8_t channels) {
    return src && src->fmt == fmt && src->channels == channels;
}

audf_node_t *audf_graph_add_input(audf_graph_t *graph, audf_fmt_t fmt, uint8_t channels) {
    return add(graph, NODE_INPUT, NULL, 0, fmt, channels);
}

audf_node_t *audf_graph_add_source(audf_graph_t *graph, audf_fmt_t fmt, uint8_t channels,
                                   audf_source_read_t read, void *user) {
    if (!read) return fail(graph, ESP_ERR_INVALID_ARG);
    audf_node_t *node = add(graph, NODE_SOURCE, NULL, 0, fmt, channels);
    if (node) {
        node->source_read = read;
        node->user = user;
    }
    return node;
}

audf_node_t *audf_graph_add_fifo(audf_graph_t *graph, audf_node_t *src, audf_fifo_t *fifo) {
    if (!fifo) return fail(graph, ESP_ERR_INVALID_ARG);
    audf_fmt_t fmt = audf_fifo_fmt(fifo);
    uint8_t channels = audf_fifo_channels(fifo);
    if (src && !matches(src, fmt, channels)) return fail(graph, ESP_ERR_INVALID_ARG);
    audf_node_t *node = add(graph, NODE_FIFO, src ? &src : NULL, src ? 1 : 0, fmt, channels);
    if (node) node->fifo = fifo;
    return node;
}

audf_node_t *audf_graph_add_eq(audf_graph_t *graph, audf_node_t *src, audf_eq_t *eq) {
    if (!eq || !matches(src, audf_eq_fmt(eq), audf_eq_channels(eq))) return fail(graph, ESP_ERR_INVALID_ARG);
    audf_node_t *node = add(graph, NODE_EQ, &src, 1, src->fmt, src->channels);
    if (node) node->eq = eq;
    return node;
}

audf_node_t *audf_graph_add_gain(audf_graph_t *graph, audf_node_t *src, audf_gain_t *gain) {
    if (!gain || !matches(src, audf_gain_fmt(gain), audf_gain_channels(gain))) return fail(graph, ESP_ERR_INVALID_ARG);
    audf_node_t *node = add(graph, NODE_GAIN, &src, 1, src->fmt, src->channels);
    if (node) node->gain = gain;
    return node;
}

audf_node_t *audf_graph_add_convert(audf_graph_t *graph, audf_node_t *src, audf_fmt_t fmt) {
    if (!src) return fail(graph, ESP_ERR_INVALID_ARG);
    return add(graph, NODE_CONVERT, &src, 1, fmt, src->channels);
}

audf_node_t *audf_graph_add_mixer(audf_graph_t *graph, audf_node_t *const srcs[], audf_mixer_t *mixer) {
    if (!mixer || !srcs) return fail(graph, ESP_ERR_INVALID_ARG);
    uint8_t n = audf_mixer_num_inputs(mixer);
    audf_fmt_t fmt = audf_mixer_fmt(mixer);
    for (uint8_t i = 0; i < n; i++) {
        if (!matches(srcs[i], fmt, audf_mixer_in_channels(mixer, i))) return fail(graph, ESP_ERR_INVALID_ARG);
    }
    audf_node_t *node = add(graph, NODE_MIXER, srcs, n, fmt, audf_mixer_out_channels(mixer));
    if (node) node->mixer = mixer;
    return node;
}

audf_node_t *audf_graph_add_resampler(audf_graph_t *graph, audf_node_t *src,
                                      audf_resampler_t *resampler, audf_drift_t *drift) {
    if (!resampler || !matches(src, audf_resampler_fmt(resampler), audf_resampler_channels(resampler))) {
        return fail(graph, ESP_ERR_INVALID_ARG);
    }
    audf_fifo_t *drift_fifo = NULL;
    if (drift) {
        for (audf_node_t *up = src; up; up = up->num_srcs == 1 ? up->srcs[0] : NULL) {
            if (up->kind == NODE_FIFO) {
                drift_fifo = up->fifo;
                break;
            }
        }
        if (!drift_fifo) return fail(graph, ESP_ERR_INVALID_ARG);
    }
    audf_node_t *node = add(graph, NODE_RESAMPLER, &src, 1, src->fmt, src->channels);
    if (node) {
        node->resampler = resampler;
        node->drift = drift;
        node->drift_fifo = drift_fifo;
    }
    return node;
}

audf_node_t *audf_graph_add_node(audf_graph_t *graph, audf_node_t *const srcs[], size_t num_srcs,
                                 audf_fmt_t fmt, uint8_t channels,
                                 const audf_node_ops_t *ops, void *ctx) {
    if (!ops || !ops->process || !num_srcs || !srcs) return fail(graph, ESP_ERR_INVALID_ARG);
    audf_node_t *node = add(graph, NODE_CUSTOM, srcs, num_srcs, fmt, channels);
    if (node) {
        node->ops = ops;
        node->user = ctx;
    }
    return node;
}

audf_node_t *audf_graph_add_sink(audf_graph_t *graph, audf_node_t *src, audf_sink_write_t write, void *user) {
    if (!src || !write) return fail(graph, ESP_ERR_INVALID_ARG);
    audf_node_t *node = add(graph, NODE_SINK, &src, 1, src->fmt, src->channels);
    if (node) {
        node->sink_write = write;
        node->user = user;
    }
    return node;
}

static esp_err_t link_consumers(audf_graph_t *graph) {
    for (audf_node_t *node = graph->head; node; node = node->next) {
        for (size_t i = 0; i < node->num_srcs; i++) node->srcs[i]->num_consumers++;
    }
    for (audf_node_t *node = graph->head; node; node = node->next) {
        if (!node->num_consumers) continue;
        node->consumers = audf_malloc(node->num_consumers * sizeof(audf_node_t *), graph->caps);
        if (!node->consumers) return ESP_ERR_NO_MEM;
        node->num_consumers = 0;
    }
    for (audf_node_t *node = graph->head; node; node = node->next) {
        for (size_t i = 0; i < node->num_srcs; i++) {
            audf_node_t *src = node->srcs[i];
            src->consumers[src->num_consumers++] = node;
        }
    }
    return ESP_OK;
}

static esp_err_t assign_direction(audf_node_t *node) {
    switch (node->kind) {
        case NODE_INPUT:
            node->push = true;
            return ESP_OK;
        case NODE_SOURCE:
            node->push = false;
            return ESP_OK;
        case NODE_FIFO:
            node->push = false;
            return node->num_srcs && !node->srcs[0]->push ? ESP_ERR_INVALID_ARG : ESP_OK;
        case NODE_SINK:
            node->push = true;
            return node->srcs[0]->push ? ESP_OK : ESP_ERR_INVALID_ARG;
        case NODE_MIXER:
        case NODE_CUSTOM: {
            size_t pushed = 0;
            for (size_t i = 0; i < node->num_srcs; i++) pushed += node->srcs[i]->push;
            node->push = pushed == 1;
            return pushed > 1 ? ESP_ERR_INVALID_ARG : ESP_OK;
        }
        default:
            node->push = node->srcs[0]->push;
            return ESP_OK;
    }
}

static size_t push_out_cap(const audf_node_t *node, size_t in_cap) {
    return node->kind == NODE_RESAMPLER ? audf_resampler_max_out(node->resampler, in_cap) : in_cap;
}

static size_t push_in_cap(const audf_node_t *node) {
    for (size_t i = 0; i < node->num_srcs; i++) {
        if (node->srcs[i]->push) return node->srcs[i]->cap;
    }
    return 0;
}

static size_t pull_demand(const audf_graph_t *graph, const audf_node_t *node) {
    if (!node->num_consumers) return graph->max_frames;
    const audf_node_t *c = node->consumers[0];
    if (c->push) return push_in_cap(c);
    if (c->kind == NODE_RESAMPLER) return audf_resampler_max_in(c->resampler, c->cap);
    return c->cap;
}

static esp_err_t size_buffers(audf_graph_t *graph) {
    for (audf_node_t *node = graph->head; node; node = node->next) {
        if (!node->push) continue;
        node->cap = node->kind == NODE_INPUT ? graph->max_frames : push_out_cap(node, push_in_cap(node));
    }
    audf_node_t **order = audf_malloc(graph->count * sizeof(audf_node_t *), graph->caps);
    if (!order) return ESP_ERR_NO_MEM;
    size_t n = 0;
    for (audf_node_t *node = graph->head; node; node = node->next) order[n++] = node;
    while (n--) {
        audf_node_t *node = order[n];
        if (node->push) continue;
        node->cap = pull_demand(graph, node);
    }
    audf_free(order);

    for (audf_node_t *node = graph->head; node; node = node->next) {
        bool needs_buf;
        switch (node->kind) {
            case NODE_SINK:
                needs_buf = false;
                break;
            case NODE_FIFO:
                needs_buf = !node->push;
                break;
            case NODE_INPUT:
                needs_buf = false;
                for (size_t i = 0; i < node->num_consumers; i++) {
                    if (node->consumers[i]->kind == NODE_SINK) needs_buf = true;
                }
                break;
            default:
                needs_buf = true;
                break;
        }
        if (node->kind == NODE_RESAMPLER) {
            size_t in = node->push ? push_in_cap(node) : audf_resampler_max_in(node->resampler, node->cap);
            if (in > audf_resampler_max_in_frames(node->resampler)) return ESP_ERR_INVALID_SIZE;
        }
        if (!needs_buf || !node->cap) continue;
        node->buf = audf_malloc(node->cap * audf_frame_bytes(node->fmt, node->channels), graph->caps);
        if (!node->buf) return ESP_ERR_NO_MEM;
    }
    return ESP_OK;
}

esp_err_t audf_graph_build(audf_graph_t *graph) {
    if (!graph) return ESP_ERR_INVALID_ARG;
    if (graph->err != ESP_OK) return graph->err;
    if (graph->built) return ESP_ERR_INVALID_STATE;
    esp_err_t err = link_consumers(graph);
    for (audf_node_t *node = graph->head; node && err == ESP_OK; node = node->next) {
        err = assign_direction(node);
    }
    for (audf_node_t *node = graph->head; node && err == ESP_OK; node = node->next) {
        if (!node->push && node->num_consumers > 1) err = ESP_ERR_INVALID_ARG;
    }
    if (err == ESP_OK) err = size_buffers(graph);
    if (err != ESP_OK) {
        graph->err = err;
        return err;
    }
    for (audf_node_t *node = graph->head; node; node = node->next) {
        if (node->drift_fifo) node->drift_generation = audf_fifo_generation(node->drift_fifo);
    }
    graph->built = true;
    return ESP_OK;
}

static esp_err_t pull(audf_node_t *node, size_t frames);

static esp_err_t gather(audf_node_t *node, const audf_node_t *from, const void *data, size_t frames) {
    for (size_t i = 0; i < node->num_srcs; i++) {
        audf_node_t *src = node->srcs[i];
        if (src == from) {
            node->ins[i] = data;
            continue;
        }
        esp_err_t err = pull(src, frames);
        if (err != ESP_OK) return err;
        node->ins[i] = src->buf;
    }
    return ESP_OK;
}

static esp_err_t process(audf_node_t *node, size_t frames) {
    switch (node->kind) {
        case NODE_EQ:
            audf_eq_process(node->eq, node->ins[0], node->buf, frames);
            return ESP_OK;
        case NODE_GAIN:
            audf_gain_process(node->gain, node->ins[0], node->buf, frames);
            return ESP_OK;
        case NODE_CONVERT:
            audf_convert(node->ins[0], node->srcs[0]->fmt, node->buf, node->fmt, frames * node->channels);
            return ESP_OK;
        case NODE_MIXER:
            audf_mixer_process(node->mixer, node->ins, node->buf, frames);
            return ESP_OK;
        case NODE_CUSTOM:
            return node->ops->process(node->user, node->ins, node->buf, frames);
        default:
            return ESP_ERR_INVALID_STATE;
    }
}

static void steer(audf_node_t *node) {
    if (!node->drift) return;
    uint32_t generation = audf_fifo_generation(node->drift_fifo);
    if (generation != node->drift_generation) {
        node->drift_generation = generation;
        audf_drift_reset(node->drift);
        audf_resampler_set_adjust(node->resampler, 0.0f);
    }
    if (!audf_fifo_primed(node->drift_fifo)) return;
    size_t level = audf_fifo_level(node->drift_fifo);
    audf_resampler_set_adjust(node->resampler, audf_drift_update(node->drift, level, node->drift_elapsed));
}

static esp_err_t pull(audf_node_t *node, size_t frames) {
    esp_err_t err;
    switch (node->kind) {
        case NODE_SOURCE: {
            size_t n = node->source_read(node->user, node->buf, frames);
            if (n > frames) n = frames;
            size_t fb = audf_frame_bytes(node->fmt, node->channels);
            memset((uint8_t *)node->buf + n * fb, 0, (frames - n) * fb);
            return ESP_OK;
        }
        case NODE_FIFO:
            return audf_fifo_read(node->fifo, node->buf, frames);
        case NODE_RESAMPLER: {
            steer(node);
            size_t need = audf_resampler_frames_needed(node->resampler, frames);
            node->drift_elapsed = need;
            audf_node_t *src = node->srcs[0];
            err = pull(src, need);
            if (err != ESP_OK) return err;
            size_t n = frames;
            err = audf_resampler_process(node->resampler, src->buf, need, node->buf, &n);
            if (err != ESP_OK) return err;
            size_t fb = audf_frame_bytes(node->fmt, node->channels);
            memset((uint8_t *)node->buf + n * fb, 0, (frames - n) * fb);
            return ESP_OK;
        }
        default:
            err = gather(node, NULL, NULL, frames);
            if (err != ESP_OK) return err;
            return process(node, frames);
    }
}

static esp_err_t deliver(audf_node_t *node, const void *data, size_t frames);

static esp_err_t feed(audf_node_t *node, const audf_node_t *from, const void *data, size_t frames) {
    esp_err_t err;
    switch (node->kind) {
        case NODE_FIFO:
            return audf_fifo_write(node->fifo, data, frames);
        case NODE_SINK:
            return node->sink_write(node->user, (void *)data, frames);
        case NODE_RESAMPLER: {
            size_t n = node->cap;
            err = audf_resampler_process(node->resampler, data, frames, node->buf, &n);
            if (err != ESP_OK) return err;
            return n ? deliver(node, node->buf, n) : ESP_OK;
        }
        default:
            err = gather(node, from, data, frames);
            if (err != ESP_OK) return err;
            err = process(node, frames);
            if (err != ESP_OK) return err;
            return deliver(node, node->buf, frames);
    }
}

static esp_err_t deliver(audf_node_t *node, const void *data, size_t frames) {
    for (size_t i = 0; i < node->num_consumers; i++) {
        esp_err_t err = feed(node->consumers[i], node, data, frames);
        if (err != ESP_OK) return err;
    }
    return ESP_OK;
}

esp_err_t audf_graph_write(audf_graph_t *graph, audf_node_t *input, const void *data, size_t frames) {
    if (!graph || !input || (frames && !data) || input->kind != NODE_INPUT) return ESP_ERR_INVALID_ARG;
    if (!graph->built) return ESP_ERR_INVALID_STATE;
    const uint8_t *src = data;
    size_t fb = audf_frame_bytes(input->fmt, input->channels);
    while (frames) {
        size_t n = frames < graph->max_frames ? frames : graph->max_frames;
        const void *chunk = src;
        if (input->buf) {
            memcpy(input->buf, src, n * fb);
            chunk = input->buf;
        }
        esp_err_t err = deliver(input, chunk, n);
        if (err != ESP_OK) return err;
        src += n * fb;
        frames -= n;
    }
    return ESP_OK;
}

esp_err_t audf_graph_read(audf_graph_t *graph, audf_node_t *node, void *data, size_t frames) {
    if (!graph || !node || (frames && !data)) return ESP_ERR_INVALID_ARG;
    if (!graph->built) return ESP_ERR_INVALID_STATE;
    if (node->push || node->num_consumers) return ESP_ERR_INVALID_ARG;
    uint8_t *dst = data;
    size_t fb = audf_frame_bytes(node->fmt, node->channels);
    while (frames) {
        size_t n = frames < node->cap ? frames : node->cap;
        esp_err_t err = pull(node, n);
        if (err != ESP_OK) return err;
        memcpy(dst, node->buf, n * fb);
        dst += n * fb;
        frames -= n;
    }
    return ESP_OK;
}
