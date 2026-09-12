/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host transport: commands on stdin, replies on stdout (the simulator logs to
 * stderr, so stdout carries only protocol lines). EOF on a piped stdin ends
 * the run like `quit`, so a driver closing the pipe never leaves a headless
 * simulator behind.
 */

#include "harness_internal.h"

#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "bsp_harness.h"

static pthread_mutex_t s_out_mtx = PTHREAD_MUTEX_INITIALIZER;

static int stdio_write(const void *data, size_t len) {
    pthread_mutex_lock(&s_out_mtx);
    size_t n = fwrite(data, 1, len, stdout);
    fflush(stdout);
    pthread_mutex_unlock(&s_out_mtx);
    return (int)n;
}

static void *reader_main(void *arg) {
    (void)arg;
    char line[512];
    while (fgets(line, sizeof(line), stdin)) harness_handle_line(line);
    if (!isatty(STDIN_FILENO)) harness_request_quit();
    return NULL;
}

static bool screenshot_cb(const char *path, void *user) {
    (void)user;
    return harness_capture_to_file(path, 90);
}

esp_err_t harness_transport_start(void) {
    harness_set_writer(stdio_write);
    bsp_harness_set_screenshot_cb(screenshot_cb, NULL);
    pthread_t t;
    if (pthread_create(&t, NULL, reader_main, NULL) != 0) return ESP_FAIL;
    pthread_detach(t);
    return ESP_OK;
}
