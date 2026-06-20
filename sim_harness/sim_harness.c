/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Sim harness core (see sim_harness.h). A line-oriented script interpreter that
 * runs on its own thread and steps in lockstep with the host's main loop: every
 * sim_harness_frame() bumps a frame counter and wakes the interpreter, so wait /
 * settle advance exactly as many real frames as the loop ran. All UI pumping and
 * presentation stay in the main loop; this core only injects input and triggers
 * capture through the callbacks the platform registered. It depends on nothing
 * but pthreads + libc — no SDL, LVGL, BSP, or app symbols.
 */

#include "sim_harness.h"

#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* MARK: injected capabilities + command registry */

static sim_harness_touch_down_fn s_touch_down;
static sim_harness_touch_up_fn   s_touch_up;
static sim_harness_capture_fn    s_capture;

#define SIM_HARNESS_MAX_CMDS 32
static struct {
    const char *name;
    sim_cmd_fn  fn;
    void       *user;
} s_cmds[SIM_HARNESS_MAX_CMDS];
static int s_cmd_count;

void sim_harness_set_input_callback(sim_harness_touch_down_fn down,
                                    sim_harness_touch_up_fn   up) {
    s_touch_down = down;
    s_touch_up   = up;
}

void sim_harness_set_capture_callback(sim_harness_capture_fn capture) {
    s_capture = capture;
}

bool sim_harness_register(const char *name, sim_cmd_fn fn, void *user) {
    if (!name || !fn || s_cmd_count >= SIM_HARNESS_MAX_CMDS) return false;
    s_cmds[s_cmd_count].name = name;
    s_cmds[s_cmd_count].fn   = fn;
    s_cmds[s_cmd_count].user = user;
    s_cmd_count++;
    return true;
}

/* MARK: frame handshake (main loop <-> interpreter thread) */

static pthread_mutex_t s_mtx  = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t  s_cond = PTHREAD_COND_INITIALIZER;
static uint64_t        s_frame_seq;   /* bumped by each sim_harness_frame() */
static bool            s_last_idle;   /* idle flag carried by the latest frame */
static bool            s_active;      /* a scripted run is in progress */
static bool            s_finished;    /* interpreter reached quit / EOF */
static int             s_exit_code;

static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000 + (uint64_t)ts.tv_nsec / 1000000;
}

/* Block until the main loop posts the next frame; return that frame's idle flag. */
static bool wait_one_frame(void) {
    pthread_mutex_lock(&s_mtx);
    uint64_t seen = s_frame_seq;
    while (s_frame_seq == seen) pthread_cond_wait(&s_cond, &s_mtx);
    bool idle = s_last_idle;
    pthread_mutex_unlock(&s_mtx);
    return idle;
}

/* MARK: built-in commands */

#define TOUCH_HOLD_MS 80   /* each tap edge must span >= one LVGL indev read */

static void do_wait(uint32_t ms) {
    uint64_t start = now_ms();
    do { wait_one_frame(); } while (now_ms() - start < ms);
}

static void do_settle(uint32_t max_ms) {
    uint64_t start = now_ms();
    int quiet = 0;
    while (now_ms() - start < max_ms) {
        if (wait_one_frame()) { if (++quiet >= 4) return; }   /* ~4 quiet frames */
        else quiet = 0;
    }
}

static void do_capture(const char *path) {
    if (!s_capture) { fprintf(stderr, "[sim] capture: no capture callback\n"); return; }
    if (!s_capture(path)) fprintf(stderr, "[sim] capture: failed (%s)\n", path);
}

static void do_touch_down(int id, int x, int y) {
    if (s_touch_down) s_touch_down(id, x, y);
    else fprintf(stderr, "[sim] touch: no input callback\n");
}

static void do_touch_up(int id) {
    if (s_touch_up) s_touch_up(id);
    else fprintf(stderr, "[sim] touch: no input callback\n");
}

static void do_tap(int x, int y, int id) {
    do_touch_down(id, x, y);
    do_wait(TOUCH_HOLD_MS);
    do_touch_up(id);
    do_wait(TOUCH_HOLD_MS);
}

/* MARK: line interpreter */

#define MAX_ARGS 8

/* Execute one script line (modified in place by tokenization). Returns false to
 * stop the run. */
static bool run_line(char *line) {
    char *argv[MAX_ARGS];
    int argc = 0;
    for (char *p = line; argc < MAX_ARGS; ) {
        while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
        if (*p == '\0') break;
        argv[argc++] = p;
        while (*p && *p != ' ' && *p != '\t' && *p != '\n' && *p != '\r') p++;
        if (*p) *p++ = '\0';
    }
    if (argc == 0 || argv[0][0] == '#') return true;

    const char *cmd = argv[0];
    if (!strcmp(cmd, "quit")) {
        return false;
    } else if (!strcmp(cmd, "wait")) {
        do_wait(argc >= 2 ? (uint32_t)atoi(argv[1]) : 0);
    } else if (!strcmp(cmd, "settle")) {
        do_settle(argc >= 2 ? (uint32_t)atoi(argv[1]) : 5000);
    } else if (!strcmp(cmd, "capture")) {
        if (argc >= 2) do_capture(argv[1]);
        else fprintf(stderr, "[sim] capture: missing path\n");
    } else if (!strcmp(cmd, "tap")) {
        if (argc >= 3) do_tap(atoi(argv[1]), atoi(argv[2]), argc >= 4 ? atoi(argv[3]) : 0);
        else fprintf(stderr, "[sim] tap: need x y\n");
    } else if (!strcmp(cmd, "down") || !strcmp(cmd, "move")) {
        if (argc >= 3) do_touch_down(argc >= 4 ? atoi(argv[3]) : 0, atoi(argv[1]), atoi(argv[2]));
        else fprintf(stderr, "[sim] %s: need x y\n", cmd);
    } else if (!strcmp(cmd, "up")) {
        do_touch_up(argc >= 2 ? atoi(argv[1]) : 0);
    } else {
        for (int i = 0; i < s_cmd_count; i++) {
            if (!strcmp(cmd, s_cmds[i].name))
                return s_cmds[i].fn(argc, (const char *const *)argv, s_cmds[i].user);
        }
        fprintf(stderr, "[sim] unknown command: %s\n", cmd);
    }
    return true;
}

static void finish(int code) {
    pthread_mutex_lock(&s_mtx);
    s_exit_code = code;
    s_finished = true;
    pthread_cond_broadcast(&s_cond);
    pthread_mutex_unlock(&s_mtx);
}

static void *interp_main(void *arg) {
    char *path = (char *)arg;
    FILE *f = (strcmp(path, "-") == 0) ? stdin : fopen(path, "r");
    if (!f) {
        fprintf(stderr, "[sim] cannot open script: %s\n", path);
        free(path);
        finish(1);
        return NULL;
    }
    char line[1100];
    while (fgets(line, sizeof(line), f)) {
        if (!run_line(line)) break;
    }
    if (f != stdin) fclose(f);
    free(path);
    finish(0);
    return NULL;
}

/* MARK: run control */

void sim_harness_start(const char *script_path) {
    if (!script_path || !*script_path) return;   /* interactive run */
    s_active = true;
    char *dup = strdup(script_path);
    pthread_t t;
    if (pthread_create(&t, NULL, interp_main, dup) != 0) {
        fprintf(stderr, "[sim] failed to start interpreter thread\n");
        free(dup);
        s_active = false;
        return;
    }
    pthread_detach(t);
}

bool sim_harness_frame(bool idle) {
    if (!s_active) return true;
    pthread_mutex_lock(&s_mtx);
    s_frame_seq++;
    s_last_idle = idle;
    pthread_cond_broadcast(&s_cond);
    bool finished = s_finished;
    pthread_mutex_unlock(&s_mtx);
    return !finished;
}

int sim_harness_exit_code(void) {
    return s_exit_code;
}
