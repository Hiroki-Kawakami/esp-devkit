/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "audf_types.h"
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    AUDF_FIFO_UNDERRUN_BLOCK = 0,
    AUDF_FIFO_UNDERRUN_SILENCE,
} audf_fifo_underrun_t;

typedef enum {
    AUDF_FIFO_OVERRUN_BLOCK = 0,
    AUDF_FIFO_OVERRUN_DROP_OLDEST,
} audf_fifo_overrun_t;

typedef struct audf_fifo audf_fifo_t;

/* Reads deliver nothing until prefill frames are queued, and running dry
 * re-arms the prefill. */
typedef struct {
    audf_fmt_t fmt;
    uint8_t    channels;
    size_t     capacity;        /*!< frames */
    size_t     prefill;         /*!< frames; must not exceed capacity */
    audf_fifo_underrun_t underrun;
    audf_fifo_overrun_t  overrun;
} audf_fifo_config_t;

esp_err_t audf_fifo_create(const audf_fifo_config_t *config, audf_fifo_t **out);
void      audf_fifo_destroy(audf_fifo_t *fifo);

/* One writer and one reader. Both return INVALID_STATE once aborted. */
esp_err_t audf_fifo_write(audf_fifo_t *fifo, const void *data, size_t frames);
esp_err_t audf_fifo_read(audf_fifo_t *fifo, void *data, size_t frames);

size_t audf_fifo_level(audf_fifo_t *fifo);
bool   audf_fifo_primed(audf_fifo_t *fifo);

/* Drops everything queued and re-arms the prefill. */
void audf_fifo_flush(audf_fifo_t *fifo);
/* Wakes a blocked writer and reader for teardown. */
void audf_fifo_abort(audf_fifo_t *fifo);

#ifdef __cplusplus
}
#endif
