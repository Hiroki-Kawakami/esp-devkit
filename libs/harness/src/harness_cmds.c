/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Built-in commands over the bsp_harness.h hooks. The touch table mirrors what
 * a multi-touch controller reports: every down/move/up re-emits the full set of
 * held contacts, so the app sees the same frames a real panel would send.
 */

#include "harness_internal.h"

#include <stdlib.h>
#include <string.h>

#include "bsp.h"
#include "bsp_harness.h"

#define HARNESS_MAX_TOUCH 10
#define B64_LINE_RAW      3072   /* raw bytes per "#D" line (-> 4096 base64 chars) */

static struct {
    bool active;
    int  id, x, y;
} s_touch[HARNESS_MAX_TOUCH];

static void touch_emit(void) {
    bsp_touch_point_t pts[HARNESS_MAX_TOUCH];
    int n = 0;
    for (int i = 0; i < HARNESS_MAX_TOUCH; i++) {
        if (!s_touch[i].active) continue;
        pts[n].x  = s_touch[i].x;
        pts[n].y  = s_touch[i].y;
        pts[n].id = s_touch[i].id;
        n++;
    }
    bsp_harness_touch_inject(pts, n);
}

static void touch_set(int id, bool pressed, int x, int y) {
    int found = -1, free_slot = -1;
    for (int i = 0; i < HARNESS_MAX_TOUCH; i++) {
        if (s_touch[i].active && s_touch[i].id == id) { found = i; break; }
        if (free_slot < 0 && !s_touch[i].active) free_slot = i;
    }
    if (pressed) {
        int slot = found >= 0 ? found : free_slot;
        if (slot < 0) return;
        s_touch[slot].active = true;
        s_touch[slot].id = id;
        s_touch[slot].x  = x;
        s_touch[slot].y  = y;
    } else if (found >= 0) {
        s_touch[found].active = false;
    }
    touch_emit();
}

static bool cmd_ping(int argc, const char *const *argv, void *user) {
    (void)argc; (void)argv; (void)user;
    harness_reply("OK ping");
    return true;
}

static const char *pixfmt_name(bsp_pixel_format_t f) {
    switch (f) {
        case BSP_PIXEL_FORMAT_L8:     return "l8";
        case BSP_PIXEL_FORMAT_RGB888: return "rgb888";
        default:                      return "rgb565";
    }
}

static bool cmd_info(int argc, const char *const *argv, void *user) {
    (void)argc; (void)argv; (void)user;
    const bsp_size_t size = bsp_display_get_size();
    harness_reply("OK info %d %d %s %d %u %d",
                  size.width, size.height, pixfmt_name(bsp_display_get_pixel_format()),
                  bsp_harness_touch_present() ? 1 : 0, (unsigned)bsp_button_count(),
                  (bsp_display_get_caps() & BSP_DISPLAY_CAP_READBACK) ? 1 : 0);
    return true;
}

static bool cmd_touch_down(int argc, const char *const *argv, void *user) {
    (void)user;
    if (argc < 4) { harness_reply("ERR %s: need id x y", argv[0]); return true; }
    touch_set(atoi(argv[1]), true, atoi(argv[2]), atoi(argv[3]));
    return true;
}

static bool cmd_touch_up(int argc, const char *const *argv, void *user) {
    (void)user;
    if (argc < 2) { harness_reply("ERR up: need id"); return true; }
    touch_set(atoi(argv[1]), false, 0, 0);
    return true;
}

static bool cmd_btn(int argc, const char *const *argv, void *user) {
    (void)user;
    if (argc < 3) { harness_reply("ERR btn: need id down|up"); return true; }
    bool pressed;
    if (!strcmp(argv[2], "down"))    pressed = true;
    else if (!strcmp(argv[2], "up")) pressed = false;
    else { harness_reply("ERR btn: down|up"); return true; }
    esp_err_t err = bsp_harness_button_inject((uint8_t)atoi(argv[1]), pressed);
    if (err != ESP_OK) { harness_reply("ERR btn: %s", esp_err_to_name(err)); return true; }
    return true;
}

static bool cmd_idle(int argc, const char *const *argv, void *user) {
    (void)argc; (void)argv; (void)user;
    harness_reply("OK idle %d", harness_is_idle() ? 1 : 0);
    return true;
}

/* MARK: snap — JPEG streamed as base64 lines */

typedef struct {
    char    line[3 + (B64_LINE_RAW / 3) * 4 + 1];   /* "#D " + base64 + '\n' */
    size_t  len;                                   /* base64 chars staged */
    uint8_t carry[3];
    int     carry_n;
    size_t  total;
} b64_sink_t;

static const char kB64[] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

static void b64_flush_line(b64_sink_t *s) {
    if (s->len == 0) return;
    memcpy(s->line, "#D ", 3);
    s->line[3 + s->len] = '\n';
    harness_write(s->line, 3 + s->len + 1);
    s->len = 0;
}

static void b64_put3(b64_sink_t *s, const uint8_t *p, int n) {
    uint32_t v = ((uint32_t)p[0] << 16) | ((uint32_t)(n > 1 ? p[1] : 0) << 8) | (n > 2 ? p[2] : 0);
    char *o = s->line + 3 + s->len;
    o[0] = kB64[(v >> 18) & 63];
    o[1] = kB64[(v >> 12) & 63];
    o[2] = n > 1 ? kB64[(v >> 6) & 63] : '=';
    o[3] = n > 2 ? kB64[v & 63] : '=';
    s->len += 4;
    if (s->len >= (B64_LINE_RAW / 3) * 4) b64_flush_line(s);
}

static int b64_sink_write(void *user, const void *data, size_t n) {
    b64_sink_t *s = user;
    const uint8_t *p = data;
    size_t i = 0;
    s->total += n;
    while (s->carry_n > 0 && s->carry_n < 3 && i < n) s->carry[s->carry_n++] = p[i++];
    if (s->carry_n == 3) { b64_put3(s, s->carry, 3); s->carry_n = 0; }
    for (; i + 3 <= n; i += 3) b64_put3(s, p + i, 3);
    while (i < n) s->carry[s->carry_n++] = p[i++];
    return (int)n;
}

static void b64_sink_finish(b64_sink_t *s) {
    if (s->carry_n) { b64_put3(s, s->carry, s->carry_n); s->carry_n = 0; }
    b64_flush_line(s);
}

static bool cmd_snap(int argc, const char *const *argv, void *user) {
    (void)user;
    const int quality = argc >= 2 ? atoi(argv[1]) : 0;
    if (!(bsp_display_get_caps() & BSP_DISPLAY_CAP_READBACK)) {
        harness_reply("ERR snap: no readback on this panel");
        return true;
    }
    b64_sink_t *s = calloc(1, sizeof(*s));
    if (!s) { harness_reply("ERR snap: no mem"); return true; }

    const bsp_size_t size = bsp_display_get_size();
    harness_reply("OK snap %d %d", size.width, size.height);
    imgf_sink_t sink = { b64_sink_write, s };
    esp_err_t err = harness_capture(sink, quality);
    b64_sink_finish(s);
    if (err == ESP_OK) harness_reply("END %u", (unsigned)s->total);
    else               harness_reply("ERR snap: %s", esp_err_to_name(err));
    free(s);
    return true;
}

void harness_cmds_register(void) {
    harness_register("ping", cmd_ping,       NULL);
    harness_register("info", cmd_info,       NULL);
    harness_register("down", cmd_touch_down, NULL);
    harness_register("move", cmd_touch_down, NULL);
    harness_register("up",   cmd_touch_up,   NULL);
    harness_register("btn",  cmd_btn,        NULL);
    harness_register("idle", cmd_idle,       NULL);
    harness_register("snap", cmd_snap,       NULL);
}

esp_err_t harness_start(void) {
    harness_cmds_register();
    return harness_transport_start();
}
