/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * PC tuning link on the log console (client: tool/index.html over Web Serial).
 * Line-based commands in, single-line replies out, prefixed '#' so the client
 * can tell them from log noise ('#OK ...' / '#ERR <msg>'):
 *
 *   ping                          liveness probe
 *   info                          -> #OK info <w> <h> <tweak 0|1> <stepmax>
 *   wf <slot> <steps> <hex>       rebind a waveform LUT (paper_s3 only);
 *                                 hex = steps*16 uint32 words, 8 hex chars each
 *   dump <slot>                   -> #OK dump <slot> <steps> <hex>
 *   refresh <fast|quality> [all]  full-screen one-shot refresh
 *   clear                         bsp_display_clear()
 *   pattern                       (re)load the test-pattern screen
 *   img begin <bytes>             start a JPEG/PNG upload
 *   img data <base64>             append a chunk (each chunk is ACKed)
 *   img end                       decode -> resize -> dither -> show
 *   img abort                     drop the upload
 */

#include "epd_tweak.hpp"
#include "lvgl.hpp"
#include "screen_manager.hpp"
#include "TestPatternScreen.hpp"
#include "ImageScreen.hpp"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "sdkconfig.h"
#include <cassert>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>

#include "imgf_alloc.h"
#include "imgf_dither.h"
#include "imgf_resize.h"
#include "imgf_sniff.h"

#if EPD_LL_TWEAK
#include "epd_ll_tweak.h"
#include "epd_waveform.h"
#endif

#if CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG
#include "driver/usb_serial_jtag.h"
#include "driver/usb_serial_jtag_vfs.h"
#elif CONFIG_ESP_CONSOLE_UART_DEFAULT || CONFIG_ESP_CONSOLE_UART_CUSTOM
#include "driver/uart.h"
#include "driver/uart_vfs.h"
#endif

static const char *TAG = "serial_cmd";

// wf line for 62 steps is ~8KB; img data chunks are ~3KB.
static constexpr size_t kLineMax = 16 * 1024;
static constexpr size_t kImgMax  = 4 * 1024 * 1024;

static void console_rx_init() {
#if CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG
    usb_serial_jtag_driver_config_t cfg = {
        .tx_buffer_size = 4096,
        .rx_buffer_size = 16384,
    };
    ESP_ERROR_CHECK(usb_serial_jtag_driver_install(&cfg));
    usb_serial_jtag_vfs_use_driver();
#elif CONFIG_ESP_CONSOLE_UART_DEFAULT || CONFIG_ESP_CONSOLE_UART_CUSTOM
    ESP_ERROR_CHECK(uart_driver_install((uart_port_t)CONFIG_ESP_CONSOLE_UART_NUM,
                                        16384, 4096, 0, NULL, 0));
    uart_vfs_dev_use_driver(CONFIG_ESP_CONSOLE_UART_NUM);
#else
#error "epd_tweak serial link needs a UART or USB-Serial-JTAG console"
#endif
    setvbuf(stdin, NULL, _IONBF, 0);
}

/* One fwrite per reply so a concurrent log line can't interleave mid-reply. */
static void reply(const char *fmt, ...) {
    static char buf[256];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(buf, sizeof(buf) - 1, fmt, ap);
    va_end(ap);
    if (n < 0) return;
    if (n > (int)sizeof(buf) - 2) n = sizeof(buf) - 2;
    buf[n] = '\n';
    fwrite(buf, 1, (size_t)n + 1, stdout);
    fflush(stdout);
}

static char *next_tok(char **cursor) {
    char *p = *cursor;
    while (*p == ' ') p++;
    if (!*p) return NULL;
    char *start = p;
    while (*p && *p != ' ') p++;
    if (*p) *p++ = 0;
    *cursor = p;
    return start;
}

// MARK: - waveform LUT swap (paper_s3 / epd_ll only)

#if EPD_LL_TWEAK

/* Double-buffered per slot: the bound table must stay untouched until the
 * engine has switched to its replacement. */
typedef uint32_t lut_table_t[EPD_WF_STEP_MAX][16];
static lut_table_t *s_lut_pool;
static uint8_t      s_lut_flip[EPD_LL_WAVEFORM_COUNT];

static bool parse_hex_words(const char *hex, uint32_t *out, size_t nwords) {
    for (size_t i = 0; i < nwords; i++) {
        uint32_t v = 0;
        for (int k = 0; k < 8; k++) {
            char c = *hex++;
            uint32_t d;
            if (c >= '0' && c <= '9')      d = (uint32_t)(c - '0');
            else if (c >= 'a' && c <= 'f') d = (uint32_t)(c - 'a' + 10);
            else if (c >= 'A' && c <= 'F') d = (uint32_t)(c - 'A' + 10);
            else return false;
            v = (v << 4) | d;
        }
        out[i] = v;
    }
    return *hex == 0;
}

static void cmd_wf(char **cur) {
    char *slot_s = next_tok(cur), *steps_s = next_tok(cur), *hex = next_tok(cur);
    if (!slot_s || !steps_s || !hex) { reply("#ERR wf: usage: wf <slot> <steps> <hex>"); return; }
    int slot = atoi(slot_s), steps = atoi(steps_s);
    if (slot < 0 || slot >= EPD_LL_WAVEFORM_COUNT) { reply("#ERR wf: bad slot"); return; }
    if (steps < 1 || steps > EPD_WF_STEP_MAX)      { reply("#ERR wf: bad steps"); return; }
    if (strlen(hex) != (size_t)steps * 16 * 8)     { reply("#ERR wf: hex length mismatch"); return; }

    if (!s_lut_pool) {
        s_lut_pool = (lut_table_t *)heap_caps_malloc(
            sizeof(lut_table_t) * EPD_LL_WAVEFORM_COUNT * 2, MALLOC_CAP_SPIRAM);
        if (!s_lut_pool) { reply("#ERR wf: no mem"); return; }
    }
    lut_table_t *dst = &s_lut_pool[slot * 2 + (s_lut_flip[slot] ^ 1)];
    if (!parse_hex_words(hex, &(*dst)[0][0], (size_t)steps * 16)) {
        reply("#ERR wf: bad hex");
        return;
    }
    esp_err_t err = epd_ll_tweak_set_waveform_lut((epd_ll_waveform_t)slot, *dst, (size_t)steps);
    if (err != ESP_OK) { reply("#ERR wf: %s", esp_err_to_name(err)); return; }
    s_lut_flip[slot] ^= 1;
    reply("#OK wf %d %d", slot, steps);
}

static void cmd_dump(char **cur) {
    char *slot_s = next_tok(cur);
    if (!slot_s) { reply("#ERR dump: usage: dump <slot>"); return; }
    int slot = atoi(slot_s);
    if (slot < 0 || slot >= EPD_LL_WAVEFORM_COUNT) { reply("#ERR dump: bad slot"); return; }

    epd_ll_lut_t lut = NULL;
    size_t steps = 0;
    esp_err_t err = epd_ll_tweak_get_waveform_lut((epd_ll_waveform_t)slot, &lut, &steps);
    if (err != ESP_OK)   { reply("#ERR dump: %s", esp_err_to_name(err)); return; }
    if (!lut || !steps)  { reply("#OK dump %d 0", slot); return; }

    static char *out;
    if (!out) out = (char *)heap_caps_malloc(kLineMax, MALLOC_CAP_SPIRAM);
    if (!out) { reply("#ERR dump: no mem"); return; }
    int n = snprintf(out, kLineMax, "#OK dump %d %u ", slot, (unsigned)steps);
    for (size_t f = 0; f < steps; f++) {
        for (int i = 0; i < 16; i++) {
            n += snprintf(out + n, kLineMax - (size_t)n, "%08lx", (unsigned long)lut[f][i]);
        }
    }
    out[n++] = '\n';
    fwrite(out, 1, (size_t)n, stdout);
    fflush(stdout);
}

#else  /* !EPD_LL_TWEAK (paper / IT8951E build) */

static void cmd_wf(char **cur)   { (void)cur; reply("#ERR wf: unsupported on this board"); }
static void cmd_dump(char **cur) { (void)cur; reply("#ERR dump: unsupported on this board"); }

#endif

// MARK: - display ops

static void cmd_refresh(char **cur) {
    char *mode_s = next_tok(cur), *all_s = next_tok(cur);
    bsp_epd_mode_t mode;
    if (mode_s && !strcmp(mode_s, "fast"))         mode = BSP_EPD_MODE_FAST;
    else if (mode_s && !strcmp(mode_s, "quality")) mode = BSP_EPD_MODE_QUALITY;
    else if (mode_s && !strcmp(mode_s, "text"))    mode = BSP_EPD_MODE_TEXT;
    else { reply("#ERR refresh: usage: refresh <fast|quality|text> [all]"); return; }
    if (all_s && !strcmp(all_s, "all")) mode = (bsp_epd_mode_t)(mode | BSP_EPD_MODE_ALL);

    bsp_rect_t full = { { 0, 0 }, bsp_display_get_size() };
    bsp_display_refresh(full, mode);
    reply("#OK refresh");
}

static void cmd_clear() {
    bsp_display_clear();
    reply("#OK clear");
}

static void cmd_pattern() {
    lv_async_call([]() {
        screen_manager.load(std::make_shared<TestPatternScreen>());
    });
    reply("#OK pattern");
}

// MARK: - image upload

static uint8_t *s_img;
static size_t   s_img_cap, s_img_len;
static uint32_t s_b64_acc;
static int      s_b64_bits;

static void img_reset() {
    if (s_img) heap_caps_free(s_img);
    s_img = NULL;
    s_img_cap = s_img_len = 0;
    s_b64_acc = 0;
    s_b64_bits = 0;
}

static bool img_append_b64(const char *s) {
    for (; *s; s++) {
        char c = *s;
        int v;
        if (c >= 'A' && c <= 'Z')      v = c - 'A';
        else if (c >= 'a' && c <= 'z') v = c - 'a' + 26;
        else if (c >= '0' && c <= '9') v = c - '0' + 52;
        else if (c == '+')             v = 62;
        else if (c == '/')             v = 63;
        else if (c == '=')             continue;
        else return false;
        s_b64_acc = (s_b64_acc << 6) | (uint32_t)v;
        s_b64_bits += 6;
        if (s_b64_bits >= 8) {
            s_b64_bits -= 8;
            if (s_img_len >= s_img_cap) return false;
            s_img[s_img_len++] = (uint8_t)(s_b64_acc >> s_b64_bits);
        }
    }
    return true;
}

/* decode -> Gray8 contain-fit -> 16-level dither -> centered on a white
 * panel-sized canvas, handed to ImageScreen on the LVGL context. */
static void img_show() {
    const bsp_size_t panel = bsp_display_get_size();

    imgf_format_t fmt = imgf_sniff(s_img, s_img_len < 8 ? s_img_len : 8);
    imgf_decoder_t *dec = imgf_make_decoder(fmt);
    if (!dec) { reply("#ERR img: unsupported format"); return; }

    imgf_buffer_source_t src_state;
    imgf_stream_t stream = imgf_stream_from_buffer(&src_state, s_img, s_img_len);
    imgf_decode_opts_t dopts = {};
    dopts.target_w   = (uint16_t)panel.width;
    dopts.target_h   = (uint16_t)panel.height;
    dopts.alloc_caps = MALLOC_CAP_SPIRAM;
    imgf_err_t ierr = imgf_decoder_open(dec, stream, &dopts);
    if (ierr != IMGF_OK) {
        imgf_decoder_destroy(dec);
        reply("#ERR img: open: %s", imgf_err_to_str(ierr));
        return;
    }

    imgf_resize_opts_t ropts = {};
    ropts.target_w   = (uint16_t)panel.width;
    ropts.target_h   = (uint16_t)panel.height;
    ropts.fit        = IMGF_FIT_CONTAIN;
    ropts.dst_pixfmt = IMGF_PIX_GRAY8;
    ropts.alloc_caps = MALLOC_CAP_SPIRAM;
    uint8_t *gray = NULL;
    uint16_t w = 0, h = 0;
    ierr = imgf_resize_decoder(dec, &ropts, &gray, &w, &h);
    imgf_decoder_destroy(dec);
    if (ierr != IMGF_OK) { reply("#ERR img: resize: %s", imgf_err_to_str(ierr)); return; }

    imgf_dither_opts_t dth = {};
    dth.algo       = IMGF_DITHER_FLOYD_STEINBERG;
    dth.levels     = 16;
    dth.out_mode   = IMGF_DITHER_OUT_GRAY;
    dth.serpentine = true;
    ierr = imgf_dither_buffer(gray, w, h, 0, gray, 0, &dth);
    if (ierr != IMGF_OK) {
        imgf_free(gray);
        reply("#ERR img: dither: %s", imgf_err_to_str(ierr));
        return;
    }

    uint8_t *fb = (uint8_t *)heap_caps_aligned_alloc(
        4, (size_t)panel.width * panel.height, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!fb) {
        imgf_free(gray);
        reply("#ERR img: no mem");
        return;
    }
    memset(fb, 0xFF, (size_t)panel.width * panel.height);
    const int ox = (panel.width - w) / 2, oy = (panel.height - h) / 2;
    for (int y = 0; y < h; y++) {
        memcpy(fb + (size_t)(oy + y) * panel.width + ox, gray + (size_t)y * w, w);
    }
    imgf_free(gray);

    lv_async_call([fb]() {
        screen_manager.load(std::make_shared<ImageScreen>(fb));
    });
    reply("#OK img %dx%d", w, h);
}

static void cmd_img(char **cur) {
    char *sub = next_tok(cur);
    if (!sub) { reply("#ERR img: usage: img <begin|data|end|abort>"); return; }

    if (!strcmp(sub, "begin")) {
        char *size_s = next_tok(cur);
        long size = size_s ? atol(size_s) : 0;
        if (size <= 0 || (size_t)size > kImgMax) { reply("#ERR img: bad size"); return; }
        img_reset();
        s_img = (uint8_t *)heap_caps_malloc((size_t)size, MALLOC_CAP_SPIRAM);
        if (!s_img) { reply("#ERR img: no mem"); return; }
        s_img_cap = (size_t)size;
        reply("#OK img begin");
    } else if (!strcmp(sub, "data")) {
        char *b64 = next_tok(cur);
        if (!s_img)  { reply("#ERR img: no upload in progress"); return; }
        if (!b64)    { reply("#ERR img: missing data"); return; }
        if (!img_append_b64(b64)) { img_reset(); reply("#ERR img: bad data/overflow"); return; }
        reply("#OK img data %u", (unsigned)s_img_len);
    } else if (!strcmp(sub, "end")) {
        if (!s_img) { reply("#ERR img: no upload in progress"); return; }
        if (s_img_len != s_img_cap) {
            img_reset();
            reply("#ERR img: size mismatch");
            return;
        }
        img_show();
        img_reset();
    } else if (!strcmp(sub, "abort")) {
        img_reset();
        reply("#OK img abort");
    } else {
        reply("#ERR img: unknown subcommand");
    }
}

// MARK: - command loop

static void handle_line(char *line) {
    char *cur = line;
    char *cmd = next_tok(&cur);
    if (!cmd) return;

    if (!strcmp(cmd, "ping")) {
        reply("#OK ping");
    } else if (!strcmp(cmd, "info")) {
        bsp_size_t size = bsp_display_get_size();
#if EPD_LL_TWEAK
        reply("#OK info %d %d 1 %d", size.width, size.height, EPD_WF_STEP_MAX);
#else
        reply("#OK info %d %d 0 0", size.width, size.height);
#endif
    } else if (!strcmp(cmd, "wf"))      { cmd_wf(&cur); }
    else if (!strcmp(cmd, "dump"))      { cmd_dump(&cur); }
    else if (!strcmp(cmd, "refresh"))   { cmd_refresh(&cur); }
    else if (!strcmp(cmd, "clear"))     { cmd_clear(); }
    else if (!strcmp(cmd, "pattern"))   { cmd_pattern(); }
    else if (!strcmp(cmd, "img"))       { cmd_img(&cur); }
    else                                { reply("#ERR unknown command: %s", cmd); }
}

static void serial_task(void *) {
    console_rx_init();
    ESP_LOGI(TAG, "serial tuning link ready");

    char *line = (char *)heap_caps_malloc(kLineMax, MALLOC_CAP_SPIRAM);
    assert(line);
    size_t len = 0;
    for (;;) {
        int c = fgetc(stdin);
        if (c == EOF) { vTaskDelay(pdMS_TO_TICKS(20)); continue; }
        if (c == '\n' || c == '\r') {
            if (len) {
                line[len] = 0;
                handle_line(line);
                len = 0;
            }
            continue;
        }
        if (len < kLineMax - 1) line[len++] = (char)c;
        else len = 0;   /* oversized line: drop it whole */
    }
}

void serial_cmd_start() {
    xTaskCreatePinnedToCore(serial_task, "serial_cmd", 12288, NULL, 3, NULL, 1);
}
