/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Seams between the harness core, its built-in commands, the capture path and
 * the per-target transport.
 */

#pragma once
#include <stdbool.h>
#include <stddef.h>
#include "harness.h"
#include "imgf_stream.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Transport -> core: one received line (modified in place while tokenizing). */
void harness_handle_line(char *line);

/* Transport registers how reply bytes leave; returns bytes written. */
typedef int (*harness_write_fn)(const void *data, size_t len);
void harness_set_writer(harness_write_fn write);
int  harness_write(const void *data, size_t len);

/* Core state the built-ins read. */
bool harness_is_idle(void);
void harness_lock(void);
void harness_unlock(void);
void harness_request_quit(void);

/* Built-ins register themselves into the same table as app commands. */
void harness_cmds_register(void);

/* JPEG-encode the whole panel into `sink` (takes the app lock while reading).
 * quality 0 -> encoder default. */
esp_err_t harness_capture(imgf_sink_t sink, int quality);

/* Per-target transport start (console task / stdin thread). */
esp_err_t harness_transport_start(void);

#ifdef __cplusplus
}
#endif
