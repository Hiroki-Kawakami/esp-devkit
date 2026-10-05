/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "audf_fifo.h"
#include <stdlib.h>
#include <string.h>
#include "audf_alloc.h"
#include "audf_internal.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

struct audf_fifo {
    SemaphoreHandle_t lock;
    SemaphoreHandle_t readable;
    SemaphoreHandle_t writable;
    audf_fifo_config_t config;
    size_t   frame_bytes;
    uint8_t *buf;
    size_t   head;
    size_t   count;
    bool     primed;
    bool     aborted;
    uint32_t generation;
};

esp_err_t audf_fifo_create(const audf_fifo_config_t *config, audf_fifo_t **out) {
    if (!config || !out || !config->capacity || config->prefill > config->capacity) return ESP_ERR_INVALID_ARG;
    if (config->channels < 1 || config->channels > AUDF_MAX_CHANNELS) return ESP_ERR_INVALID_ARG;
    audf_fifo_t *fifo = audf_calloc(1, sizeof(*fifo), config->alloc_caps);
    if (!fifo) return ESP_ERR_NO_MEM;
    fifo->config = *config;
    fifo->frame_bytes = audf_frame_bytes(config->fmt, config->channels);
    fifo->buf = audf_malloc(config->capacity * fifo->frame_bytes, config->alloc_caps);
    fifo->lock = xSemaphoreCreateMutex();
    fifo->readable = xSemaphoreCreateBinary();
    fifo->writable = xSemaphoreCreateBinary();
    if (!fifo->buf || !fifo->lock || !fifo->readable || !fifo->writable) {
        audf_fifo_destroy(fifo);
        return ESP_ERR_NO_MEM;
    }
    fifo->primed = config->prefill == 0;
    *out = fifo;
    return ESP_OK;
}

void audf_fifo_destroy(audf_fifo_t *fifo) {
    if (!fifo) return;
    if (fifo->lock) vSemaphoreDelete(fifo->lock);
    if (fifo->readable) vSemaphoreDelete(fifo->readable);
    if (fifo->writable) vSemaphoreDelete(fifo->writable);
    audf_free(fifo->buf);
    audf_free(fifo);
}

static void copy_in(audf_fifo_t *fifo, const uint8_t *src, size_t frames) {
    size_t cap = fifo->config.capacity;
    size_t tail = (fifo->head + fifo->count) % cap;
    size_t first = frames < cap - tail ? frames : cap - tail;
    memcpy(fifo->buf + tail * fifo->frame_bytes, src, first * fifo->frame_bytes);
    memcpy(fifo->buf, src + first * fifo->frame_bytes, (frames - first) * fifo->frame_bytes);
    fifo->count += frames;
}

static void copy_out(audf_fifo_t *fifo, uint8_t *dst, size_t frames) {
    size_t cap = fifo->config.capacity;
    size_t first = frames < cap - fifo->head ? frames : cap - fifo->head;
    memcpy(dst, fifo->buf + fifo->head * fifo->frame_bytes, first * fifo->frame_bytes);
    memcpy(dst + first * fifo->frame_bytes, fifo->buf, (frames - first) * fifo->frame_bytes);
    fifo->head = (fifo->head + frames) % cap;
    fifo->count -= frames;
}

static void drop(audf_fifo_t *fifo, size_t frames) {
    fifo->head = (fifo->head + frames) % fifo->config.capacity;
    fifo->count -= frames;
}

esp_err_t audf_fifo_write(audf_fifo_t *fifo, const void *data, size_t frames) {
    if (!fifo || (frames && !data)) return ESP_ERR_INVALID_ARG;
    const uint8_t *src = data;
    size_t cap = fifo->config.capacity;
    xSemaphoreTake(fifo->lock, portMAX_DELAY);
    if (fifo->config.overrun == AUDF_FIFO_OVERRUN_DROP_OLDEST && frames > cap) {
        src += (frames - cap) * fifo->frame_bytes;
        frames = cap;
    }
    while (frames) {
        if (fifo->aborted) {
            xSemaphoreGive(fifo->lock);
            return ESP_ERR_INVALID_STATE;
        }
        size_t space = cap - fifo->count;
        if (fifo->config.overrun == AUDF_FIFO_OVERRUN_DROP_OLDEST && space < frames) {
            drop(fifo, frames - space);
            space = frames;
        }
        if (!space) {
            xSemaphoreGive(fifo->lock);
            xSemaphoreTake(fifo->writable, portMAX_DELAY);
            xSemaphoreTake(fifo->lock, portMAX_DELAY);
            continue;
        }
        size_t n = frames < space ? frames : space;
        copy_in(fifo, src, n);
        src += n * fifo->frame_bytes;
        frames -= n;
        if (!fifo->primed && fifo->count >= fifo->config.prefill) fifo->primed = true;
        xSemaphoreGive(fifo->readable);
    }
    xSemaphoreGive(fifo->lock);
    return ESP_OK;
}

esp_err_t audf_fifo_read(audf_fifo_t *fifo, void *data, size_t frames) {
    if (!fifo || (frames && !data)) return ESP_ERR_INVALID_ARG;
    uint8_t *dst = data;
    xSemaphoreTake(fifo->lock, portMAX_DELAY);
    while (frames) {
        if (fifo->aborted) {
            xSemaphoreGive(fifo->lock);
            return ESP_ERR_INVALID_STATE;
        }
        if (fifo->primed) {
            size_t n = frames < fifo->count ? frames : fifo->count;
            copy_out(fifo, dst, n);
            dst += n * fifo->frame_bytes;
            frames -= n;
            if (n) xSemaphoreGive(fifo->writable);
            if (!frames) break;
            fifo->primed = fifo->config.prefill == 0;
        }
        if (fifo->config.underrun == AUDF_FIFO_UNDERRUN_SILENCE) {
            memset(dst, 0, frames * fifo->frame_bytes);
            break;
        }
        xSemaphoreGive(fifo->lock);
        xSemaphoreTake(fifo->readable, portMAX_DELAY);
        xSemaphoreTake(fifo->lock, portMAX_DELAY);
    }
    xSemaphoreGive(fifo->lock);
    return ESP_OK;
}

size_t audf_fifo_level(audf_fifo_t *fifo) {
    xSemaphoreTake(fifo->lock, portMAX_DELAY);
    size_t count = fifo->count;
    xSemaphoreGive(fifo->lock);
    return count;
}

bool audf_fifo_primed(audf_fifo_t *fifo) {
    xSemaphoreTake(fifo->lock, portMAX_DELAY);
    bool primed = fifo->primed;
    xSemaphoreGive(fifo->lock);
    return primed;
}

void audf_fifo_flush(audf_fifo_t *fifo) {
    xSemaphoreTake(fifo->lock, portMAX_DELAY);
    fifo->head = 0;
    fifo->count = 0;
    fifo->primed = fifo->config.prefill == 0;
    fifo->generation++;
    xSemaphoreGive(fifo->writable);
    xSemaphoreGive(fifo->lock);
}

void audf_fifo_abort(audf_fifo_t *fifo) {
    xSemaphoreTake(fifo->lock, portMAX_DELAY);
    fifo->aborted = true;
    xSemaphoreGive(fifo->readable);
    xSemaphoreGive(fifo->writable);
    xSemaphoreGive(fifo->lock);
}

audf_fmt_t audf_fifo_fmt(const audf_fifo_t *fifo) { return fifo->config.fmt; }
uint8_t audf_fifo_channels(const audf_fifo_t *fifo) { return fifo->config.channels; }

uint32_t audf_fifo_generation(audf_fifo_t *fifo) {
    xSemaphoreTake(fifo->lock, portMAX_DELAY);
    uint32_t generation = fifo->generation;
    xSemaphoreGive(fifo->lock);
    return generation;
}
