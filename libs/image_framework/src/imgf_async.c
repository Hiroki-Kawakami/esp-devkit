/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Producer/consumer two-task pipeline runner. ESP-IDF path uses FreeRTOS
 * tasks + semaphores (core-pinned per opts); host path uses pthread + mutex
 * + condvar with the same ring semantics.
 */

#include "imgf_async.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "imgf_alloc.h"

#ifdef ESP_PLATFORM
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "freertos/semphr.h"
#else
#include <pthread.h>
#endif

#define IMGF_ASYNC_DEFAULT_SLOTS        4
#define IMGF_ASYNC_DEFAULT_STACK_WORDS  2048
#define IMGF_ASYNC_DEFAULT_PRIO         5

/* ---- ring ------------------------------------------------------------- */

struct imgf_async_ring {
    uint8_t *buf;
    size_t   row_bytes;
    int      slots;
    int      head;        /* next put */
    int      tail;        /* next get */
    int      count;       /* items currently in ring */
    uint8_t  closed;
    uint8_t  aborted;

#ifdef ESP_PLATFORM
    SemaphoreHandle_t mutex;
    SemaphoreHandle_t not_full;   /* binary: signaled when room or aborted */
    SemaphoreHandle_t not_empty;  /* binary: signaled when row ready, closed, or aborted */
#else
    pthread_mutex_t mutex;
    pthread_cond_t  cv_not_full;
    pthread_cond_t  cv_not_empty;
#endif
};

/* Lock helpers — both targets present the same shape so the put/get
 * functions don't fork. */

static void lock(imgf_async_ring_t *r) {
#ifdef ESP_PLATFORM
    xSemaphoreTake(r->mutex, portMAX_DELAY);
#else
    pthread_mutex_lock(&r->mutex);
#endif
}

static void unlock(imgf_async_ring_t *r) {
#ifdef ESP_PLATFORM
    xSemaphoreGive(r->mutex);
#else
    pthread_mutex_unlock(&r->mutex);
#endif
}

#ifdef ESP_PLATFORM
/* On FreeRTOS we use binary semaphores as edge-triggered "wakeup" signals;
 * the actual condition (count/closed/aborted) is re-checked under the mutex.
 * This matches the host condvar pattern and avoids tracking counting
 * semaphore values that drift relative to closed/aborted state. */
static void wait_release(imgf_async_ring_t *r, SemaphoreHandle_t sem) {
    unlock(r);
    xSemaphoreTake(sem, portMAX_DELAY);
    lock(r);
}
static void signal_one(SemaphoreHandle_t sem) { xSemaphoreGive(sem); }
#else
static void wait_release(imgf_async_ring_t *r, pthread_cond_t *cv) {
    pthread_cond_wait(cv, &r->mutex);
}
static void signal_all(pthread_cond_t *cv) { pthread_cond_broadcast(cv); }
#endif

static imgf_err_t ring_init(imgf_async_ring_t *r, size_t row_bytes, int slots,
                            uint32_t alloc_caps) {
    memset(r, 0, sizeof *r);
    r->row_bytes = row_bytes;
    r->slots     = slots;
    r->buf = (uint8_t *)imgf_alloc((size_t)slots * row_bytes, alloc_caps);
    if (!r->buf) return IMGF_ERR_OOM;
#ifdef ESP_PLATFORM
    r->mutex     = xSemaphoreCreateMutex();
    r->not_full  = xSemaphoreCreateBinary();
    r->not_empty = xSemaphoreCreateBinary();
    if (!r->mutex || !r->not_full || !r->not_empty) {
        if (r->mutex)     vSemaphoreDelete(r->mutex);
        if (r->not_full)  vSemaphoreDelete(r->not_full);
        if (r->not_empty) vSemaphoreDelete(r->not_empty);
        imgf_free(r->buf);
        return IMGF_ERR_OOM;
    }
#else
    pthread_mutex_init(&r->mutex, NULL);
    pthread_cond_init(&r->cv_not_full,  NULL);
    pthread_cond_init(&r->cv_not_empty, NULL);
#endif
    return IMGF_OK;
}

static void ring_deinit(imgf_async_ring_t *r) {
#ifdef ESP_PLATFORM
    vSemaphoreDelete(r->mutex);
    vSemaphoreDelete(r->not_full);
    vSemaphoreDelete(r->not_empty);
#else
    pthread_mutex_destroy(&r->mutex);
    pthread_cond_destroy(&r->cv_not_full);
    pthread_cond_destroy(&r->cv_not_empty);
#endif
    if (r->buf) imgf_free(r->buf);
}

imgf_err_t imgf_async_ring_put(imgf_async_ring_t *r, const void *row) {
    lock(r);
    while (r->count == r->slots && !r->aborted) {
#ifdef ESP_PLATFORM
        wait_release(r, r->not_full);
#else
        wait_release(r, &r->cv_not_full);
#endif
    }
    if (r->aborted) { unlock(r); return IMGF_ERR_INVALID_STATE; }
    memcpy(r->buf + (size_t)r->head * r->row_bytes, row, r->row_bytes);
    r->head = (r->head + 1) % r->slots;
    r->count++;
#ifdef ESP_PLATFORM
    signal_one(r->not_empty);
#else
    signal_all(&r->cv_not_empty);
#endif
    unlock(r);
    return IMGF_OK;
}

int imgf_async_ring_get(imgf_async_ring_t *r, void *out) {
    lock(r);
    while (r->count == 0 && !r->closed && !r->aborted) {
#ifdef ESP_PLATFORM
        wait_release(r, r->not_empty);
#else
        wait_release(r, &r->cv_not_empty);
#endif
    }
    if (r->count == 0) {
        int rc = r->aborted ? -1 : 0;
        unlock(r);
        return rc;
    }
    memcpy(out, r->buf + (size_t)r->tail * r->row_bytes, r->row_bytes);
    r->tail = (r->tail + 1) % r->slots;
    r->count--;
#ifdef ESP_PLATFORM
    signal_one(r->not_full);
#else
    signal_all(&r->cv_not_full);
#endif
    unlock(r);
    return 1;
}

static void ring_close(imgf_async_ring_t *r) {
    lock(r);
    r->closed = 1;
#ifdef ESP_PLATFORM
    signal_one(r->not_empty);
#else
    signal_all(&r->cv_not_empty);
#endif
    unlock(r);
}

void imgf_async_ring_abort(imgf_async_ring_t *r) {
    lock(r);
    r->aborted = 1;
#ifdef ESP_PLATFORM
    signal_one(r->not_empty);
    signal_one(r->not_full);
#else
    signal_all(&r->cv_not_empty);
    signal_all(&r->cv_not_full);
#endif
    unlock(r);
}

size_t imgf_async_ring_row_bytes(const imgf_async_ring_t *r) {
    return r ? r->row_bytes : 0;
}

/* ---- task plumbing ---------------------------------------------------- */

typedef struct {
    imgf_async_ring_t ring;
    imgf_async_run_t  prod_fn;
    void             *prod_user;
    imgf_async_run_t  cons_fn;
    void             *cons_user;
    imgf_err_t        prod_err;
    imgf_err_t        cons_err;

#ifdef ESP_PLATFORM
    SemaphoreHandle_t done_sem;   /* counting, 2 takes after both finish */
#else
    pthread_t prod_th, cons_th;
#endif
} async_ctx_t;

#ifdef ESP_PLATFORM
static void prod_task(void *arg) {
    async_ctx_t *c = (async_ctx_t *)arg;
    c->prod_err = c->prod_fn(c->prod_user, &c->ring);
    if (c->prod_err != IMGF_OK) imgf_async_ring_abort(&c->ring);
    ring_close(&c->ring);
    xSemaphoreGive(c->done_sem);
    vTaskDelete(NULL);
}

static void cons_task(void *arg) {
    async_ctx_t *c = (async_ctx_t *)arg;
    c->cons_err = c->cons_fn(c->cons_user, &c->ring);
    if (c->cons_err != IMGF_OK) imgf_async_ring_abort(&c->ring);
    xSemaphoreGive(c->done_sem);
    vTaskDelete(NULL);
}
#else
static void *prod_thread(void *arg) {
    async_ctx_t *c = (async_ctx_t *)arg;
    c->prod_err = c->prod_fn(c->prod_user, &c->ring);
    if (c->prod_err != IMGF_OK) imgf_async_ring_abort(&c->ring);
    ring_close(&c->ring);
    return NULL;
}

static void *cons_thread(void *arg) {
    async_ctx_t *c = (async_ctx_t *)arg;
    c->cons_err = c->cons_fn(c->cons_user, &c->ring);
    if (c->cons_err != IMGF_OK) imgf_async_ring_abort(&c->ring);
    return NULL;
}
#endif

imgf_err_t imgf_async_run(const imgf_async_opts_t *opts,
                          imgf_async_run_t producer, void *producer_user,
                          imgf_async_run_t consumer, void *consumer_user) {
    if (!opts || !producer || !consumer) return IMGF_ERR_INVALID_ARG;
    if (opts->row_bytes == 0) return IMGF_ERR_INVALID_ARG;

    int slots = opts->ring_slots > 0 ? opts->ring_slots : IMGF_ASYNC_DEFAULT_SLOTS;

    async_ctx_t *c = (async_ctx_t *)calloc(1, sizeof *c);
    if (!c) return IMGF_ERR_OOM;
    c->prod_fn   = producer;
    c->prod_user = producer_user;
    c->cons_fn   = consumer;
    c->cons_user = consumer_user;
    c->prod_err  = IMGF_OK;
    c->cons_err  = IMGF_OK;

    imgf_err_t err = ring_init(&c->ring, opts->row_bytes, slots, opts->alloc_caps);
    if (err != IMGF_OK) { free(c); return err; }

#ifdef ESP_PLATFORM
    c->done_sem = xSemaphoreCreateCounting(2, 0);
    if (!c->done_sem) { ring_deinit(&c->ring); free(c); return IMGF_ERR_OOM; }

    int      prod_prio  = opts->producer_prio  > 0 ? opts->producer_prio  : IMGF_ASYNC_DEFAULT_PRIO;
    int      cons_prio  = opts->consumer_prio  > 0 ? opts->consumer_prio  : IMGF_ASYNC_DEFAULT_PRIO;
    uint32_t prod_stack = opts->producer_stack_words > 0 ? opts->producer_stack_words : IMGF_ASYNC_DEFAULT_STACK_WORDS;
    uint32_t cons_stack = opts->consumer_stack_words > 0 ? opts->consumer_stack_words : IMGF_ASYNC_DEFAULT_STACK_WORDS;

    BaseType_t ok;
    if (opts->producer_core < 0) {
        ok = xTaskCreate(prod_task, "imgf_prod", prod_stack, c, prod_prio, NULL);
    } else {
        ok = xTaskCreatePinnedToCore(prod_task, "imgf_prod", prod_stack,
                                     c, prod_prio, NULL, opts->producer_core);
    }
    if (ok != pdPASS) {
        vSemaphoreDelete(c->done_sem);
        ring_deinit(&c->ring);
        free(c);
        return IMGF_ERR_OOM;
    }
    if (opts->consumer_core < 0) {
        ok = xTaskCreate(cons_task, "imgf_cons", cons_stack, c, cons_prio, NULL);
    } else {
        ok = xTaskCreatePinnedToCore(cons_task, "imgf_cons", cons_stack,
                                     c, cons_prio, NULL, opts->consumer_core);
    }
    if (ok != pdPASS) {
        imgf_async_ring_abort(&c->ring);
        xSemaphoreTake(c->done_sem, portMAX_DELAY);   /* producer still completes */
        vSemaphoreDelete(c->done_sem);
        ring_deinit(&c->ring);
        free(c);
        return IMGF_ERR_OOM;
    }
    xSemaphoreTake(c->done_sem, portMAX_DELAY);
    xSemaphoreTake(c->done_sem, portMAX_DELAY);
    vSemaphoreDelete(c->done_sem);
#else
    if (pthread_create(&c->prod_th, NULL, prod_thread, c) != 0) {
        ring_deinit(&c->ring); free(c); return IMGF_ERR_OOM;
    }
    if (pthread_create(&c->cons_th, NULL, cons_thread, c) != 0) {
        imgf_async_ring_abort(&c->ring);
        pthread_join(c->prod_th, NULL);
        ring_deinit(&c->ring); free(c); return IMGF_ERR_OOM;
    }
    pthread_join(c->prod_th, NULL);
    pthread_join(c->cons_th, NULL);
#endif

    err = c->prod_err != IMGF_OK ? c->prod_err : c->cons_err;
    ring_deinit(&c->ring);
    free(c);
    return err;
}
