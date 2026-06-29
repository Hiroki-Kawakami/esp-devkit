/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Two-task pipeline runner. The producer callback drives an upstream chain
 * (e.g. decoder + resize) and publishes rows of a fixed size into a bounded
 * ring; the consumer callback drains the ring through a downstream chain
 * (e.g. dither + encoder). Each runs on its own task so on ESP32-S3 the two
 * halves overlap on Core 0 / Core 1. Core affinity, priority, and stack size
 * are configurable per side.
 *
 * On host builds (non-ESP_PLATFORM) the two callbacks run on pthread threads,
 * keeping the behaviour and ordering the same so the chain can be unit-tested
 * without hardware.
 *
 * Optional module: image_framework's other pieces (decoder/resize/dither/
 * encoder) do not depend on this one; only link it in when you want the
 * pipeline split.
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "imgf_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct imgf_async_ring imgf_async_ring_t;

/* Per-side callback. Drives the upstream/downstream sub-chain and exchanges
 * rows with the ring via imgf_async_ring_put / _get. Returns IMGF_OK on
 * success; any other status is propagated by imgf_async_run after both sides
 * have completed (or been aborted). */
typedef imgf_err_t (*imgf_async_run_t)(void *user, imgf_async_ring_t *ring);

typedef struct {
    /* Per-side FreeRTOS task config. Matches the xTaskCreatePinnedToCore
     * conventions directly so callers can reuse the same values they pass
     * elsewhere in the firmware. Ignored on host (pthread defaults). */
    int      producer_core;          /* -1 = no pin; 0 / 1 = pin to that core */
    int      producer_prio;          /* 0 = default (5) */
    uint32_t producer_stack_words;   /* StackType_t depth (xTaskCreate's 3rd arg); 0 = default (2048) */
    int      consumer_core;
    int      consumer_prio;
    uint32_t consumer_stack_words;

    /* Ring config. row_bytes must be > 0. */
    size_t   row_bytes;       /* size of each row exchanged through the ring */
    int      ring_slots;      /* depth; 0 -> default (4) */
    uint32_t alloc_caps;      /* heap caps for the ring buffer */
} imgf_async_opts_t;

/* Spawn producer + consumer on two tasks, wait for both to finish, return the
 * aggregated status (producer's error if non-OK, else consumer's). The ring
 * and tasks are torn down before return. */
imgf_err_t imgf_async_run(const imgf_async_opts_t *opts,
                          imgf_async_run_t producer, void *producer_user,
                          imgf_async_run_t consumer, void *consumer_user);

/* Producer-side: publish one row (opts->row_bytes long). Blocks while the
 * ring is full. Returns IMGF_OK or IMGF_ERR_INVALID_STATE if the consumer
 * aborted. */
imgf_err_t imgf_async_ring_put  (imgf_async_ring_t *r, const void *row);

/* Consumer-side: pull one row into `out`. Returns 1 if a row was written,
 * 0 at end of stream (producer closed and ring drained), -1 on error
 * (producer aborted). Blocks while the ring is empty. */
int        imgf_async_ring_get  (imgf_async_ring_t *r, void *out);

/* Either side can abort the pipeline mid-stream. The other side's pending
 * wait returns with the error path. Safe to call from inside a callback. */
void       imgf_async_ring_abort(imgf_async_ring_t *r);

/* Optional accessor for callbacks. */
size_t     imgf_async_ring_row_bytes(const imgf_async_ring_t *r);

#ifdef __cplusplus
}
#endif
