/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GT911 HotKnot subsystem firmware loader. Mirrors the AOSP Goodix routine
 * gup_load_hotknot_system (gt9xx_update.c:3275): without this the chip can
 * detect a pair event (track id 32) but can't exchange data — the transmission
 * firmware lives in the host as a blob and is streamed into chip SRAM after
 * pairing. Holds the chip lock across the whole load (touch can't make sense of
 * the chip's I2C while SS51 is in reset).
 */

#include "gt911_hotknot.h"

#include <stddef.h>
#include <stdint.h>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "gt911-hk-fw";

extern const uint8_t gt911_hotknot_fw_blob[];
extern const size_t  gt911_hotknot_fw_blob_size;

/* 14-byte header: bytes 0..3 = big-endian body length; 4..7 = "Ghot" tag. */
#define FW_HEADER_LEN  14
#define BANK_LENGTH    (16 * 1024)   /* 32 KB code SRAM in two 16 KB banks */
#define WRITE_CHUNK    256

/* MISCTL register set — from the AOSP GT9XX private headers. */
#define REG_MISCTL_TMR0_EN          0x40B0u
#define REG_MISCTL_DSP_CLK          0x4010u
#define REG_MISCTL_CACHE_EN         0x404Bu
#define REG_MISCTL_MEM_CD_EN        0x4049u
#define REG_MISCTL_SRAM_BANK        0x4048u
#define REG_MISCTL_SWRST_B0         0x4180u
#define REG_MISCTL_CPU_SWRST_PULSE  0x4184u
#define REG_MISCTL_BOOTCTL_B0       0x4190u
#define REG_MISCTL_BOOT_OPT_B0      0x4218u
#define REG_MISCTL_BOOT_CTL         0x5094u
#define REG_SRAM_WINDOW             0xC000u
#define REG_SW_WDT                  0x8041u

#define RETURN_ON_ERR(expr) do { esp_err_t _e = (expr); if (_e != ESP_OK) return _e; } while (0)

/* Write reg=v with bounded retries — the chip sometimes NAKs the first
 * transaction after a SWRST change while internal state catches up. */
static esp_err_t set_reg_retry(gt911_handle_t h, uint16_t reg, uint8_t v) {
    for (int i = 0; i < 5; i++) {
        if (gt911_internal_write_byte_locked(h, reg, v) == ESP_OK) return ESP_OK;
        vTaskDelay(pdMS_TO_TICKS(1));
    }
    return ESP_FAIL;
}

/* Hold SS51 + DSP cores in reset, then bring up the SRAM access path. */
static esp_err_t hold_ss51_dsp(gt911_handle_t h) {
    int retry;
    for (retry = 0; retry < 200; retry++) {
        if (gt911_internal_write_byte_locked(h, REG_MISCTL_SWRST_B0, 0x0C) != ESP_OK) continue;
        uint8_t v = 0;
        if (gt911_internal_read_reg_locked(h, REG_MISCTL_SWRST_B0, &v, 1) != ESP_OK) continue;
        if (v == 0x0C) break;
    }
    if (retry >= 200) {
        ESP_LOGE(TAG, "hold ss51+dsp failed (0x4180 readback never 0x0C)");
        return ESP_FAIL;
    }
    RETURN_ON_ERR(set_reg_retry(h, REG_MISCTL_DSP_CLK,         0x00));
    RETURN_ON_ERR(set_reg_retry(h, REG_MISCTL_TMR0_EN,         0x00));
    RETURN_ON_ERR(set_reg_retry(h, REG_MISCTL_CACHE_EN,        0x00));
    RETURN_ON_ERR(set_reg_retry(h, REG_MISCTL_BOOTCTL_B0,      0x02));  /* boot from SRAM */
    RETURN_ON_ERR(set_reg_retry(h, REG_MISCTL_CPU_SWRST_PULSE, 0x01));
    return ESP_OK;
}

/* Disable watchdog, hold cores, clear control flags, enable code-memory access. */
static esp_err_t enter_update_mode(gt911_handle_t h) {
    RETURN_ON_ERR(set_reg_retry(h, REG_MISCTL_TMR0_EN,     0x00));
    RETURN_ON_ERR(hold_ss51_dsp(h));
    RETURN_ON_ERR(set_reg_retry(h, REG_MISCTL_BOOT_CTL,    0x00));
    RETURN_ON_ERR(set_reg_retry(h, REG_MISCTL_BOOT_OPT_B0, 0x00));
    RETURN_ON_ERR(set_reg_retry(h, REG_MISCTL_MEM_CD_EN,   0x01));
    return ESP_OK;
}

/* Select a SRAM bank, stream firmware into the SRAM window. */
static esp_err_t load_bank(gt911_handle_t h, uint8_t bank, const uint8_t *fw, size_t length) {
    for (int retry = 0; retry < 5; retry++) {
        if (set_reg_retry(h, REG_MISCTL_SRAM_BANK, bank) != ESP_OK) continue;
        esp_err_t err = ESP_OK;
        for (size_t off = 0; off < length; off += WRITE_CHUNK) {
            size_t n = (length - off) < WRITE_CHUNK ? (length - off) : WRITE_CHUNK;
            err = gt911_internal_write_reg_locked(h, REG_SRAM_WINDOW + off, fw + off, n);
            if (err != ESP_OK) break;
        }
        if (err == ESP_OK) return ESP_OK;
        ESP_LOGW(TAG, "bank %d write failed (attempt %d): %s", bank, retry + 1, esp_err_to_name(err));
    }
    return ESP_FAIL;
}

/* Release the cores; the firmware starts running and clears the soft watchdog. */
static esp_err_t fw_startup(gt911_handle_t h) {
    RETURN_ON_ERR(gt911_internal_write_byte_locked(h, REG_SW_WDT,          0xAA));
    RETURN_ON_ERR(gt911_internal_write_byte_locked(h, REG_MISCTL_SWRST_B0, 0x00));
    vTaskDelay(pdMS_TO_TICKS(20));

    uint8_t wdt = 0xAA;
    RETURN_ON_ERR(gt911_internal_read_reg_locked(h, REG_SW_WDT, &wdt, 1));
    if (wdt == 0xAA) {
        ESP_LOGE(TAG, "subsystem fw did not clear 0x8041 — startup failed");
        return ESP_FAIL;
    }
    (void)gt911_internal_write_byte_locked(h, REG_SW_WDT, 0xAA);  /* re-prime for the running fw */
    return ESP_OK;
}

/* Sum of 16-bit big-endian pairs over the body must be 0 (mod 0x10000). */
static bool checksum_ok(const uint8_t *fw, size_t length) {
    uint32_t sum = 0;
    for (size_t i = 0; i + 1 < length; i += 2) sum += ((uint32_t)fw[i] << 8) | fw[i + 1];
    return (sum & 0xFFFF) == 0;
}

esp_err_t gt911_hotknot_load_subsystem(gt911_handle_t h) {
    if (!h) return ESP_ERR_INVALID_ARG;

    if (gt911_hotknot_fw_blob_size < FW_HEADER_LEN + 4096) {
        ESP_LOGE(TAG, "embedded fw blob too small: %u bytes", (unsigned)gt911_hotknot_fw_blob_size);
        return ESP_ERR_INVALID_SIZE;
    }
    uint32_t body_len = ((uint32_t)gt911_hotknot_fw_blob[0] << 24) |
                        ((uint32_t)gt911_hotknot_fw_blob[1] << 16) |
                        ((uint32_t)gt911_hotknot_fw_blob[2] << 8)  |
                                   gt911_hotknot_fw_blob[3];
    if (body_len + FW_HEADER_LEN > gt911_hotknot_fw_blob_size ||
        body_len < 4096 || body_len > 32768) {
        ESP_LOGE(TAG, "implausible fw body length: %u", (unsigned)body_len);
        return ESP_ERR_INVALID_SIZE;
    }
    const uint8_t *body = &gt911_hotknot_fw_blob[FW_HEADER_LEN];
    if (!checksum_ok(body, body_len)) {
        ESP_LOGE(TAG, "fw blob checksum mismatch — corrupted embed?");
        return ESP_ERR_INVALID_CRC;
    }

    ESP_LOGI(TAG, "loading HotKnot subsystem (%u bytes)", (unsigned)body_len);

    gt911_internal_lock(h);
    esp_err_t err = enter_update_mode(h);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "enter_update_mode failed: %s", esp_err_to_name(err));
        gt911_internal_unlock(h);
        return err;
    }

    uint8_t bank = 0;
    size_t remaining = body_len;
    while (remaining > 0) {
        size_t n = remaining > BANK_LENGTH ? BANK_LENGTH : remaining;
        err = load_bank(h, bank, body + ((size_t)bank * BANK_LENGTH), n);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "load_bank %d failed: %s", bank, esp_err_to_name(err));
            gt911_internal_unlock(h);
            return err;
        }
        ESP_LOGI(TAG, "bank %d loaded (%u bytes)", bank, (unsigned)n);
        bank++;
        remaining -= n;
    }

    err = fw_startup(h);
    gt911_internal_unlock(h);
    if (err == ESP_OK) ESP_LOGI(TAG, "HotKnot subsystem running");
    return err;
}
