/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdarg.h>
#include <stdio.h>
#include <string.h>

#include "check.h"
#include "harness.h"

#define MAX_COMMANDS 4

static struct {
    const char *name;
    harness_cmd_fn fn;
    void *user;
} s_commands[MAX_COMMANDS];

static char *s_reply;
static size_t s_reply_len;

bool harness_register(const char *name, harness_cmd_fn fn, void *user) {
    for (int i = 0; i < MAX_COMMANDS; i++) {
        if (!s_commands[i].name) {
            s_commands[i].name = name;
            s_commands[i].fn = fn;
            s_commands[i].user = user;
            return true;
        }
    }
    return false;
}

void harness_reply(const char *fmt, ...) {
    if (!s_reply) return;
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(s_reply, s_reply_len, fmt, ap);
    va_end(ap);
}

bool check_c_command(int argc, const char *const *argv, char *reply, size_t reply_len) {
    reply[0] = '\0';
    for (int i = 0; i < MAX_COMMANDS; i++) {
        if (s_commands[i].name && strcmp(s_commands[i].name, argv[0]) == 0) {
            s_reply = reply;
            s_reply_len = reply_len;
            bool ok = s_commands[i].fn(argc, argv, s_commands[i].user);
            s_reply = NULL;
            return ok;
        }
    }
    return false;
}
