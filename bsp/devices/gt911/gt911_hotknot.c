/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GT911 HotKnot: chip primitives + the BSP provider whose session state machine
 * runs on the gt911 reader task (hk_step). See gt911_hotknot.h.
 */

#include "gt911_hotknot.h"

#include <string.h>
#include "esp_check.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "gt911-hk";

/* Register map (GT911 Programming Guide §3.1, §3.5–§3.8) */
#define REG_CMD                 0x8040u
#define REG_CMD_CHECK           0x8046u  /* required for cmds > 0x07           */
#define REG_CONFIG              0x8047u  /* config block start                 */
#define REG_MODULE_SWITCH2      0x804Eu  /* bit1 HotKnot_En, bit2 Approch_En   */
#define REG_NOISE_MAP           0x80A1u
#define REG_PXY_THRESHOLD       0x80A3u
#define REG_DUMP_SHIFT          0x80A4u
#define REG_RX_GAIN             0x80A5u
#define REG_FREQ_GAIN0          0x80A6u  /* 4 bytes                            */
#define REG_GT911_STATUS        0x81A8u
#define REG_SEND_STATUS         0xAB10u  /* [Send, Rev, SendBak, RevBak]       */
#define REG_EVENT_ACK           0xAB1Fu
#define REG_TX_LEN              0xAC90u
#define REG_TX_TRIGGER          0xAD91u
#define REG_RX_STATUS           0xAE10u  /* bit7 = frame ready; +1 = length    */
#define REG_RX_DATA             0xAE12u  /* payload then CRC16-CCITT (BE)      */

#define CMD_EXIT_SLAVE          0x28u
#define CMD_EXIT_MASTER         0x29u
#define CMD_EXIT_TRANSMISSION   0x2Au

#define CONFIG_DATA_LENGTH      184
#define MODULE_SWITCH2_HOTKNOT_BITS  0x06u   /* bits the chip needs for HotKnot */
#define POLL_INTERVAL_MS        10u

/* For cmds > 0x07 the chip wants the same value in Command_Che first (ESD check). */
static esp_err_t send_command(gt911_handle_t h, uint8_t cmd) {
    esp_err_t err = gt911_write_reg(h, REG_CMD_CHECK, &cmd, 1);
    if (err != ESP_OK) return err;
    return gt911_write_reg(h, REG_CMD, &cmd, 1);
}

/* Any non-0xAA byte clears the chip's pending-event flag. */
static esp_err_t ack_event_locked(gt911_handle_t h) {
    return gt911_internal_write_byte_locked(h, REG_EVENT_ACK, 0x00);
}

/* ========================================================================
 * Config patch (HotKnot bits + board tuning)
 * ===================================================================== */

esp_err_t gt911_hotknot_enable_in_config(gt911_handle_t h) {
    ESP_RETURN_ON_FALSE(h, ESP_ERR_INVALID_ARG, TAG, "null arg");
    const gt911_hotknot_tuning_t *t = gt911_internal_tuning(h);

    uint8_t cfg[CONFIG_DATA_LENGTH + 2] = {0};  /* +checksum +fresh flag */
    esp_err_t err = gt911_read_reg(h, REG_CONFIG, cfg, CONFIG_DATA_LENGTH);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "config read failed: %s", esp_err_to_name(err));
        return err;
    }

    const int OFF_MS2  = REG_MODULE_SWITCH2 - REG_CONFIG;
    const int OFF_NMAP = REG_NOISE_MAP      - REG_CONFIG;
    const int OFF_PXY  = REG_PXY_THRESHOLD  - REG_CONFIG;
    const int OFF_DSH  = REG_DUMP_SHIFT     - REG_CONFIG;
    const int OFF_RXG  = REG_RX_GAIN        - REG_CONFIG;
    const int OFF_FRQ  = REG_FREQ_GAIN0     - REG_CONFIG;

    bool dirty = false;
    if ((cfg[OFF_MS2] & MODULE_SWITCH2_HOTKNOT_BITS) != MODULE_SWITCH2_HOTKNOT_BITS) {
        cfg[OFF_MS2] |= MODULE_SWITCH2_HOTKNOT_BITS;
        dirty = true;
    }
    if (t && t->noise_map     >= 0 && cfg[OFF_NMAP] != (uint8_t)t->noise_map)     { cfg[OFF_NMAP] = (uint8_t)t->noise_map; dirty = true; }
    if (t && t->pxy_threshold >= 0 && cfg[OFF_PXY]  != (uint8_t)t->pxy_threshold) { cfg[OFF_PXY]  = (uint8_t)t->pxy_threshold; dirty = true; }
    if (t && t->dump_shift    >= 0 && cfg[OFF_DSH]  != (uint8_t)t->dump_shift)    { cfg[OFF_DSH]  = (uint8_t)t->dump_shift; dirty = true; }
    if (t && t->rx_gain       >= 0 && cfg[OFF_RXG]  != (uint8_t)t->rx_gain)       { cfg[OFF_RXG]  = (uint8_t)t->rx_gain; dirty = true; }
    if (t && t->freq_gain     >= 0) {
        for (int i = 0; i < 4; i++)
            if (cfg[OFF_FRQ + i] != (uint8_t)t->freq_gain) { cfg[OFF_FRQ + i] = (uint8_t)t->freq_gain; dirty = true; }
    }
    if (!dirty) return ESP_OK;

    /* Two's-complement checksum so sum(data[0..183]) + chk == 0 mod 256. */
    uint8_t sum = 0;
    for (int i = 0; i < CONFIG_DATA_LENGTH; i++) sum += cfg[i];
    cfg[CONFIG_DATA_LENGTH]     = (uint8_t)(0u - sum);
    cfg[CONFIG_DATA_LENGTH + 1] = 0x01;  /* Config_Fresh: commit */

    err = gt911_write_reg(h, REG_CONFIG, cfg, CONFIG_DATA_LENGTH + 2);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "config write failed: %s", esp_err_to_name(err));
        return err;
    }
    vTaskDelay(pdMS_TO_TICKS(100));

    uint8_t ms2 = 0;
    err = gt911_read_reg(h, REG_MODULE_SWITCH2, &ms2, 1);
    if (err != ESP_OK) return err;
    if ((ms2 & MODULE_SWITCH2_HOTKNOT_BITS) != MODULE_SWITCH2_HOTKNOT_BITS) {
        ESP_LOGE(TAG, "config rejected — 0x804E=0x%02X", ms2);
        return ESP_ERR_INVALID_RESPONSE;
    }
    return ESP_OK;
}

/* ========================================================================
 * Approach mode
 * ===================================================================== */

esp_err_t gt911_hotknot_enter_approach(gt911_handle_t h, gt911_hotknot_role_t role) {
    ESP_RETURN_ON_FALSE(h, ESP_ERR_INVALID_ARG, TAG, "null arg");
    ESP_RETURN_ON_FALSE(role == GT911_HOTKNOT_ROLE_SLAVE || role == GT911_HOTKNOT_ROLE_MASTER,
                        ESP_ERR_INVALID_ARG, TAG, "bad role");
    ESP_LOGI(TAG, "enter approach (%s)", role == GT911_HOTKNOT_ROLE_MASTER ? "master" : "slave");
    return send_command(h, (uint8_t)role);
}

esp_err_t gt911_hotknot_exit(gt911_handle_t h, gt911_hotknot_mode_t current_mode) {
    ESP_RETURN_ON_FALSE(h, ESP_ERR_INVALID_ARG, TAG, "null arg");
    switch (current_mode) {
        case GT911_HOTKNOT_MODE_SLAVE:   return send_command(h, CMD_EXIT_SLAVE);
        case GT911_HOTKNOT_MODE_MASTER:  return send_command(h, CMD_EXIT_MASTER);
        case GT911_HOTKNOT_MODE_RECEIVE:
        case GT911_HOTKNOT_MODE_SEND:    return send_command(h, CMD_EXIT_TRANSMISSION);
        case GT911_HOTKNOT_MODE_IDLE:    return ESP_OK;
        default:
            (void)send_command(h, CMD_EXIT_TRANSMISSION);
            (void)send_command(h, CMD_EXIT_MASTER);
            return send_command(h, CMD_EXIT_SLAVE);
    }
}

esp_err_t gt911_hotknot_get_mode(gt911_handle_t h, gt911_hotknot_mode_t *out_mode) {
    ESP_RETURN_ON_FALSE(h && out_mode, ESP_ERR_INVALID_ARG, TAG, "null arg");
    for (int retry = 0; retry < 3; retry++) {
        uint8_t buf[2] = {0};
        esp_err_t err = gt911_read_reg(h, REG_GT911_STATUS, buf, sizeof buf);
        if (err != ESP_OK) return err;
        if (buf[0] == buf[1]) { *out_mode = (gt911_hotknot_mode_t)buf[0]; return ESP_OK; }
        vTaskDelay(pdMS_TO_TICKS(2));
    }
    *out_mode = GT911_HOTKNOT_MODE_UNKNOWN;
    return ESP_ERR_INVALID_RESPONSE;
}

/* ========================================================================
 * Data exchange
 * ===================================================================== */

/* CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF). */
static uint16_t crc16_ccitt(const uint8_t *data, size_t len) {
    uint16_t crc = 0xFFFF;
    for (size_t i = 0; i < len; i++) {
        uint8_t v = data[i];
        for (int b = 0; b < 8; b++) {
            bool flag = v & 0x80, c15 = crc & 0x8000;
            v <<= 1; crc <<= 1;
            if (c15 ^ flag) crc ^= 0x1021;
        }
    }
    return crc;
}

/* Poll [REG_SEND_STATUS..+3] until byte[status_index] matches its backup and is
 * a terminal value (OK / VERIFY_ERROR / PEER_DEPARTED). Caller holds the lock. */
static esp_err_t wait_send_status_locked(gt911_handle_t h, uint32_t timeout_ms, uint8_t *out_status) {
    TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(timeout_ms);
    for (;;) {
        uint8_t st[4] = {0};
        if (gt911_internal_read_reg_locked(h, REG_SEND_STATUS, st, sizeof st) == ESP_OK &&
            st[0] == st[2]) {
            uint8_t s = st[0];
            if (s == GT911_HOTKNOT_XFER_OK || s == GT911_HOTKNOT_XFER_VERIFY_ERROR ||
                s == GT911_HOTKNOT_XFER_PEER_DEPARTED) {
                *out_status = s;
                return ESP_OK;
            }
        }
        if ((int32_t)(deadline - xTaskGetTickCount()) <= 0) return ESP_ERR_TIMEOUT;
        vTaskDelay(pdMS_TO_TICKS(POLL_INTERVAL_MS));
    }
}

esp_err_t gt911_hotknot_send(gt911_handle_t h, const uint8_t *data, size_t len,
                             uint32_t timeout_ms, uint8_t *out_status) {
    ESP_RETURN_ON_FALSE(h && data && out_status, ESP_ERR_INVALID_ARG, TAG, "null arg");
    if (len == 0 || len > GT911_HOTKNOT_MAX_PAYLOAD || (len & 1)) return ESP_ERR_INVALID_SIZE;

    /* [len][data...][checksum], two's complement so the frame sums to 0 mod 256. */
    uint8_t frame[1 + GT911_HOTKNOT_MAX_PAYLOAD + 1];
    frame[0] = (uint8_t)len;
    memcpy(&frame[1], data, len);
    uint16_t sum = frame[0];
    for (size_t i = 0; i < len; i++) sum += data[i];
    frame[1 + len] = (uint8_t)(~sum + 1);

    gt911_internal_lock(h);
    esp_err_t err = gt911_internal_write_reg_locked(h, REG_TX_LEN, frame, 1 + len + 1);
    if (err == ESP_OK) err = gt911_internal_write_byte_locked(h, REG_TX_TRIGGER, 0xAA);
    if (err == ESP_OK) err = wait_send_status_locked(h, timeout_ms, out_status);
    if (err == ESP_OK) (void)ack_event_locked(h);   /* let the next frame start */
    gt911_internal_unlock(h);
    return err;
}

esp_err_t gt911_hotknot_poll_receive(gt911_handle_t h, uint8_t *buffer, size_t buffer_size,
                                     size_t *out_len, bool *got) {
    ESP_RETURN_ON_FALSE(h && buffer && out_len && got, ESP_ERR_INVALID_ARG, TAG, "null arg");
    *out_len = 0;
    *got     = false;

    gt911_internal_lock(h);
    uint8_t meta[2] = {0};   /* [status, length] */
    esp_err_t err = gt911_internal_read_reg_locked(h, REG_RX_STATUS, meta, sizeof meta);
    if (err != ESP_OK) { gt911_internal_unlock(h); return err; }
    if ((meta[0] & 0x80) == 0) { gt911_internal_unlock(h); return ESP_OK; }  /* nothing ready */

    uint8_t len = meta[1];
    if (len == 0 || len > GT911_HOTKNOT_MAX_PAYLOAD || len > buffer_size) {
        (void)gt911_internal_write_byte_locked(h, REG_RX_STATUS, 0x00);  /* drain so chip rearms */
        (void)ack_event_locked(h);
        gt911_internal_unlock(h);
        if (len > buffer_size) { *out_len = len; return ESP_ERR_INVALID_SIZE; }
        return ESP_ERR_INVALID_RESPONSE;
    }

    uint8_t fr[GT911_HOTKNOT_MAX_PAYLOAD + 2];   /* payload + CRC16 (BE) */
    err = gt911_internal_read_reg_locked(h, REG_RX_DATA, fr, (size_t)len + 2);
    if (err == ESP_OK) {
        (void)gt911_internal_write_byte_locked(h, REG_RX_STATUS, 0x00);
        (void)ack_event_locked(h);
    }
    gt911_internal_unlock(h);
    if (err != ESP_OK) return err;

    /* CRC over payload with the length byte appended (programming guide §3.8). */
    uint8_t crc_in[GT911_HOTKNOT_MAX_PAYLOAD + 1];
    memcpy(crc_in, fr, len);
    crc_in[len] = len;
    uint16_t want = crc16_ccitt(crc_in, (size_t)len + 1);
    uint16_t have = ((uint16_t)fr[len] << 8) | fr[len + 1];
    if (want != have) {
        ESP_LOGW(TAG, "rx crc mismatch: want=0x%04x have=0x%04x", want, have);
        return ESP_ERR_INVALID_CRC;
    }
    memcpy(buffer, fr, len);
    *out_len = len;
    *got     = true;
    return ESP_OK;
}

/* ========================================================================
 * Recover to touch
 * ===================================================================== */

esp_err_t gt911_hotknot_recover_touch(gt911_handle_t h) {
    ESP_RETURN_ON_FALSE(h, ESP_ERR_INVALID_ARG, TAG, "null arg");

    esp_err_t err = gt911_reset(h);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "gt911_reset failed: %s", esp_err_to_name(err));
        return err;
    }
    esp_err_t sync = gt911_int_sync(h);
    if (sync == ESP_ERR_NOT_SUPPORTED) {
        ESP_LOGW(TAG, "RESET/INT not fully wired — touch may need a power cycle to recover");
    } else if (sync != ESP_OK) {
        ESP_LOGW(TAG, "int_sync failed: %s", esp_err_to_name(sync));
    }
    return ESP_OK;
}

/* ========================================================================
 * BSP provider + session state machine (runs on the reader task)
 * ===================================================================== */

typedef enum { HK_IDLE, HK_APPROACH, HK_XFER, HK_FAILED } hk_phase_t;

typedef struct {
    bsp_hotknot_t          base;
    gt911_handle_t         h;
    bsp_hotknot_event_cb_t cb;
    void                  *arg;
    bsp_hotknot_role_t     role;
    volatile hk_phase_t    phase;
    uint8_t                rxbuf[BSP_HOTKNOT_MAX_PAYLOAD];
} gt911_hk_t;

static gt911_hk_t s_provider;   /* one HotKnot per board */

static void emit(gt911_hk_t *p, bsp_hotknot_event_type_t type,
                 const uint8_t *data, size_t len, esp_err_t err) {
    if (!p->cb) return;
    bsp_hotknot_event_t ev = { .type = type, .data = data, .len = len, .err = err };
    p->cb(&ev, p->arg);
}

static void hk_step(gt911_handle_t h, void *ctx) {
    gt911_hk_t *p = ctx;
    switch (p->phase) {
        case HK_APPROACH: {
            bool paired = false;
            gt911_internal_service_touch(h, &paired);   /* touch stays alive here */
            if (!paired) break;
            emit(p, BSP_HOTKNOT_EVENT_PAIRED, NULL, 0, ESP_OK);
            esp_err_t err = gt911_hotknot_load_subsystem(h);
            if (err != ESP_OK) {
                p->phase = HK_FAILED;
                emit(p, BSP_HOTKNOT_EVENT_ERROR, NULL, 0, err);
                break;
            }
            p->phase = HK_XFER;
            emit(p, BSP_HOTKNOT_EVENT_READY, NULL, 0, ESP_OK);
            break;
        }
        case HK_XFER: {
            size_t len = 0;
            bool got = false;
            esp_err_t err = gt911_hotknot_poll_receive(h, p->rxbuf, sizeof p->rxbuf, &len, &got);
            if (got) emit(p, BSP_HOTKNOT_EVENT_RECEIVED, p->rxbuf, len, ESP_OK);
            (void)err;  /* transient CRC / not-ready: just poll again next tick */
            break;
        }
        default:
            break;
    }
}

static esp_err_t hk_begin(bsp_hotknot_t *self, bsp_hotknot_role_t role,
                          bsp_hotknot_event_cb_t cb, void *arg) {
    gt911_hk_t *p = (gt911_hk_t *)self;
    if (p->phase != HK_IDLE) return ESP_ERR_INVALID_STATE;
    if (!gt911_internal_has_task(p->h)) return ESP_ERR_INVALID_STATE;

    p->cb   = cb;
    p->arg  = arg;
    p->role = role;

    esp_err_t err = gt911_hotknot_enable_in_config(p->h);
    if (err != ESP_OK) return err;
    err = gt911_hotknot_enter_approach(p->h,
            role == BSP_HOTKNOT_ROLE_MASTER ? GT911_HOTKNOT_ROLE_MASTER
                                            : GT911_HOTKNOT_ROLE_SLAVE);
    if (err != ESP_OK) return err;

    p->phase = HK_APPROACH;
    gt911_internal_set_session_step(p->h, hk_step, p);
    return ESP_OK;
}

static esp_err_t hk_send(bsp_hotknot_t *self, const void *data, size_t len, uint32_t timeout_ms) {
    gt911_hk_t *p = (gt911_hk_t *)self;
    if (p->phase != HK_XFER) return ESP_ERR_INVALID_STATE;

    uint8_t raw = 0;
    esp_err_t err = gt911_hotknot_send(p->h, data, len, timeout_ms, &raw);
    if (err != ESP_OK) return err;
    switch (raw) {
        case GT911_HOTKNOT_XFER_OK:            return ESP_OK;
        case GT911_HOTKNOT_XFER_VERIFY_ERROR:  return ESP_ERR_INVALID_CRC;
        case GT911_HOTKNOT_XFER_PEER_DEPARTED: return ESP_ERR_TIMEOUT;
        default:                               return ESP_FAIL;
    }
}

static esp_err_t hk_end(bsp_hotknot_t *self) {
    gt911_hk_t *p = (gt911_hk_t *)self;
    hk_phase_t was = p->phase;

    /* Stop scheduling steps first; a step mid-FW-load holds the chip lock, and
     * recover_touch's reset will block on it until that step returns. */
    gt911_internal_set_session_step(p->h, NULL, NULL);

    /* In approach the chip's own firmware is alive — issue the matching exit. In
     * xfer the SRAM subsystem is running and only reset+int_sync recovers. */
    if (was == HK_APPROACH) {
        gt911_hotknot_mode_t exit_mode = (p->role == BSP_HOTKNOT_ROLE_MASTER)
                                             ? GT911_HOTKNOT_MODE_MASTER
                                             : GT911_HOTKNOT_MODE_SLAVE;
        (void)gt911_hotknot_exit(p->h, exit_mode);
    }
    esp_err_t err = (was == HK_IDLE) ? ESP_OK : gt911_hotknot_recover_touch(p->h);
    p->phase = HK_IDLE;
    p->cb    = NULL;
    return err;
}

esp_err_t gt911_hotknot_create(bsp_hotknot_t **out_hk) {
    ESP_RETURN_ON_FALSE(out_hk, ESP_ERR_INVALID_ARG, TAG, "null arg");
    gt911_handle_t h = gt911_active_handle();
    if (!h) {
        ESP_LOGE(TAG, "no GT911 with a reader task — HotKnot unavailable");
        return ESP_ERR_INVALID_STATE;
    }
    s_provider.base.begin = hk_begin;
    s_provider.base.send  = hk_send;
    s_provider.base.end   = hk_end;
    s_provider.h          = h;
    s_provider.phase      = HK_IDLE;
    *out_hk = &s_provider.base;
    return ESP_OK;
}
