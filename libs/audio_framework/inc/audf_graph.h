/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Connects modules into a graph of push and pull sections. See README.md.
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "audf_drift.h"
#include "audf_eq.h"
#include "audf_fifo.h"
#include "audf_gain.h"
#include "audf_mixer.h"
#include "audf_resampler.h"
#include "audf_types.h"
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct audf_graph audf_graph_t;
typedef struct audf_node  audf_node_t;

typedef struct {
    size_t max_frames;   /*!< write/read chunk; 0 = 512 */
} audf_graph_config_t;

/* Returns the frames produced; the rest of the request plays as silence. */
typedef size_t (*audf_source_read_t)(void *user, void *data, size_t frames);
typedef esp_err_t (*audf_sink_write_t)(void *user, void *data, size_t frames);

/* A rate-preserving node; out has the node's own format. */
typedef struct {
    esp_err_t (*process)(void *ctx, const void *const in[], void *out, size_t frames);
} audf_node_ops_t;

esp_err_t audf_graph_create(const audf_graph_config_t *config, audf_graph_t **out);
/* Leaves the modules, FIFOs and drifts to their owners. */
void      audf_graph_destroy(audf_graph_t *graph);

/* add_* return NULL on failure; the error is reported by audf_graph_build. */
audf_node_t *audf_graph_add_input(audf_graph_t *graph, audf_fmt_t fmt, uint8_t channels);
audf_node_t *audf_graph_add_source(audf_graph_t *graph, audf_fmt_t fmt, uint8_t channels,
                                   audf_source_read_t read, void *user);
/* src NULL: the FIFO is fed with audf_fifo_write from outside the graph. */
audf_node_t *audf_graph_add_fifo(audf_graph_t *graph, audf_node_t *src, audf_fifo_t *fifo);
audf_node_t *audf_graph_add_eq(audf_graph_t *graph, audf_node_t *src, audf_eq_t *eq);
audf_node_t *audf_graph_add_gain(audf_graph_t *graph, audf_node_t *src, audf_gain_t *gain);
audf_node_t *audf_graph_add_convert(audf_graph_t *graph, audf_node_t *src, audf_fmt_t fmt);
/* srcs has audf_mixer_num_inputs() entries. */
audf_node_t *audf_graph_add_mixer(audf_graph_t *graph, audf_node_t *const srcs[], audf_mixer_t *mixer);
/* With a drift, the graph steers the resampler from the FIFO upstream of it. */
audf_node_t *audf_graph_add_resampler(audf_graph_t *graph, audf_node_t *src,
                                      audf_resampler_t *resampler, audf_drift_t *drift);
audf_node_t *audf_graph_add_node(audf_graph_t *graph, audf_node_t *const srcs[], size_t num_srcs,
                                 audf_fmt_t fmt, uint8_t channels,
                                 const audf_node_ops_t *ops, void *ctx);
audf_node_t *audf_graph_add_sink(audf_graph_t *graph, audf_node_t *src,
                                 audf_sink_write_t write, void *user);

/* Validates the push/pull rules and allocates. No node can be added after. */
esp_err_t audf_graph_build(audf_graph_t *graph);

/* Runs the push section from an input on the calling task. */
esp_err_t audf_graph_write(audf_graph_t *graph, audf_node_t *input, const void *data, size_t frames);
/* Pulls from a pull node that has no consumer. */
esp_err_t audf_graph_read(audf_graph_t *graph, audf_node_t *node, void *data, size_t frames);

#ifdef __cplusplus
}
#endif
