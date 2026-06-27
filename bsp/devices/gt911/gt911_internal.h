/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GT911 internal seam for the in-tree HotKnot files (gt911_hotknot*.c) — reaches
 * into the device the public touch API (gt911.h) hides. Not part of that API.
 */

#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"
#include "gt911.h"   /* gt911_config_t / gt911_hotknot_tuning_t + bsp_touch.h types */

#ifdef __cplusplus
extern "C" {
#endif

typedef struct gt911_dev *gt911_handle_t;

/* Track id 32 = a HotKnot pair event reported as a synthetic touch. */
#define GT911_HK_PAIR_TRACK_ID  32u

/* The board's GT911; NULL if none or it has no reader task (HotKnot needs it). */
gt911_handle_t gt911_active_handle(void);
bool gt911_internal_has_task(gt911_handle_t handle);

/* The _locked variants assume the caller holds the lock (FW download, send/recv);
 * gt911_read_reg/gt911_write_reg lock internally. */
void gt911_internal_lock(gt911_handle_t handle);
void gt911_internal_unlock(gt911_handle_t handle);
esp_err_t gt911_read_reg(gt911_handle_t handle, uint16_t reg, uint8_t *buf, size_t len);
esp_err_t gt911_write_reg(gt911_handle_t handle, uint16_t reg, const uint8_t *buf, size_t len);
esp_err_t gt911_internal_read_reg_locked(gt911_handle_t handle, uint16_t reg,
                                         uint8_t *buf, size_t len);
esp_err_t gt911_internal_write_reg_locked(gt911_handle_t handle, uint16_t reg,
                                          const uint8_t *buf, size_t len);
esp_err_t gt911_internal_write_byte_locked(gt911_handle_t handle, uint16_t reg, uint8_t value);

const gt911_hotknot_tuning_t *gt911_internal_tuning(gt911_handle_t handle);

/* INT level (1 = idle / no pin, 0 = asserting). */
int gt911_internal_int_level(gt911_handle_t handle);

/* One reader-task poll: emit fresh touch and, if a pair was latched, set *out_paired. */
void gt911_internal_service_touch(gt911_handle_t handle, bool *out_paired);

/* Recover touch from the HotKnot subsystem; int_sync is ESP_ERR_NOT_SUPPORTED on
 * input-only INT (then a power cycle is needed). */
esp_err_t gt911_reset(gt911_handle_t handle);
esp_err_t gt911_int_sync(gt911_handle_t handle);

/* While set, the reader task calls fn(handle, ctx) instead of its touch poll;
 * NULL restores it. HotKnot installs its state machine here. */
typedef void (*gt911_session_step_fn)(gt911_handle_t handle, void *ctx);
void gt911_internal_set_session_step(gt911_handle_t handle, gt911_session_step_fn fn, void *ctx);

#ifdef __cplusplus
}
#endif
