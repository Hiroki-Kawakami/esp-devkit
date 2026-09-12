/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Harness core: command registry, line tokenizer, reply writer and the app
 * hooks. Transport- and BSP-free so it also compiles into host unit tests
 * that only need harness_register (libs/wifi).
 */

#include "harness_internal.h"

#include <stdarg.h>
#include <stdio.h>
#include <string.h>

#define HARNESS_MAX_CMDS 32
#define HARNESS_MAX_ARGS 8

static struct {
    const char    *name;
    harness_cmd_fn fn;
    void          *user;
} s_cmds[HARNESS_MAX_CMDS];
static int s_cmd_count;

static harness_idle_fn s_idle_fn;
static void           *s_idle_user;
static harness_lock_fn s_lock_fn, s_unlock_fn;
static void           *s_lock_user;
static harness_write_fn s_write;

static volatile bool s_frame_idle;
static volatile bool s_quit;
static unsigned      s_reply_seq;   /* bumped per reply so the core can tell a handler replied */

bool harness_register(const char *name, harness_cmd_fn fn, void *user) {
    if (!name || !fn || s_cmd_count >= HARNESS_MAX_CMDS) return false;
    s_cmds[s_cmd_count].name = name;
    s_cmds[s_cmd_count].fn   = fn;
    s_cmds[s_cmd_count].user = user;
    s_cmd_count++;
    return true;
}

void harness_set_idle_cb(harness_idle_fn fn, void *user) {
    s_idle_fn   = fn;
    s_idle_user = user;
}

void harness_set_lock_cb(harness_lock_fn lock, harness_lock_fn unlock, void *user) {
    s_lock_fn   = lock;
    s_unlock_fn = unlock;
    s_lock_user = user;
}

void harness_set_writer(harness_write_fn write) {
    s_write = write;
}

int harness_write(const void *data, size_t len) {
    if (s_write) return s_write(data, len);
    size_t n = fwrite(data, 1, len, stdout);
    fflush(stdout);
    return (int)n;
}

void harness_reply(const char *fmt, ...) {
    char buf[256];
    buf[0] = '#';
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(buf + 1, sizeof(buf) - 2, fmt, ap);
    va_end(ap);
    if (n < 0) return;
    if (n > (int)sizeof(buf) - 3) n = sizeof(buf) - 3;
    buf[n + 1] = '\n';
    s_reply_seq++;
    harness_write(buf, (size_t)n + 2);
}

bool harness_is_idle(void) {
    if (s_idle_fn) return s_idle_fn(s_idle_user);
    return s_frame_idle;
}

void harness_lock(void)   { if (s_lock_fn)   s_lock_fn(s_lock_user); }
void harness_unlock(void) { if (s_unlock_fn) s_unlock_fn(s_lock_user); }

void harness_request_quit(void) { s_quit = true; }

bool harness_frame(bool idle) {
    s_frame_idle = idle;
    return !s_quit;
}

void harness_handle_line(char *line) {
    char *argv[HARNESS_MAX_ARGS];
    int argc = 0;
    for (char *p = line; argc < HARNESS_MAX_ARGS; ) {
        while (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n') p++;
        if (*p == '\0') break;
        argv[argc++] = p;
        while (*p && *p != ' ' && *p != '\t' && *p != '\r' && *p != '\n') p++;
        if (*p) *p++ = '\0';
    }
    if (argc == 0 || argv[0][0] == '#') return;

    const char *cmd = argv[0];
    if (!strcmp(cmd, "quit")) {
        harness_reply("OK quit");
        harness_request_quit();
        return;
    }
    for (int i = 0; i < s_cmd_count; i++) {
        if (strcmp(cmd, s_cmds[i].name) != 0) continue;
        unsigned before = s_reply_seq;
        bool handled = s_cmds[i].fn(argc, (const char *const *)argv, s_cmds[i].user);
        if (!handled)                  harness_reply("ERR %s: rejected", cmd);
        else if (before == s_reply_seq) harness_reply("OK %s", cmd);
        return;
    }
    harness_reply("ERR unknown command: %s", cmd);
}
