/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "host/sdio.h"

#include <string.h>

#include "driver/gpio.h"
#include "driver/sdmmc_host.h"
#include "esp_log.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "sdkconfig.h"
#include "sdmmc_cmd.h"

#define REG_TOKEN_RDATA   0x044
#define REG_INT_RAW       0x050
#define REG_PKT_LEN       0x060
#define REG_H2S_INT       0x08C
#define REG_INT_CLR       0x0D4
#define DATA_END_ADDR     0x1F800

#define CCCR_FN_ENABLE    0x02
#define CCCR_FN_READY     0x03
#define CCCR_INT_ENABLE   0x04
#define FBR_BLKSIZE(fn)   (0x100 * (fn) + 0x10)

#define SLOT              CONFIG_ESP_HOSTED_ENHANCED_SDIO_SLOT

static const char *TAG = "hosted_sdio";

static sdmmc_card_t s_card;
/* Register-sized CMD53s go through the card's one shared bounce buffer, which
 * the driver fills outside its own lock. */
static SemaphoreHandle_t s_reg_lock;

esp_err_t hosted_sdio_init(void) {
    s_reg_lock = xSemaphoreCreateMutex();
    if (!s_reg_lock) return ESP_ERR_NO_MEM;
    esp_err_t err = sdmmc_host_init();
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE) return err;

    sdmmc_slot_config_t slot = SDMMC_SLOT_CONFIG_DEFAULT();
    slot.width = 4;
    slot.clk = CONFIG_ESP_HOSTED_ENHANCED_PIN_CLK;
    slot.cmd = CONFIG_ESP_HOSTED_ENHANCED_PIN_CMD;
    slot.d0 = CONFIG_ESP_HOSTED_ENHANCED_PIN_D0;
    slot.d1 = CONFIG_ESP_HOSTED_ENHANCED_PIN_D1;
    slot.d2 = CONFIG_ESP_HOSTED_ENHANCED_PIN_D2;
    slot.d3 = CONFIG_ESP_HOSTED_ENHANCED_PIN_D3;
    slot.flags |= SDMMC_SLOT_FLAG_INTERNAL_PULLUP;
    return sdmmc_host_init_slot(SLOT, &slot);
}

static void reset_slave(void) {
#if CONFIG_ESP_HOSTED_ENHANCED_PIN_RESET >= 0
    const gpio_num_t pin = CONFIG_ESP_HOSTED_ENHANCED_PIN_RESET;
    gpio_set_direction(pin, GPIO_MODE_OUTPUT);
    gpio_set_level(pin, 0);
    vTaskDelay(pdMS_TO_TICKS(10));
    gpio_set_level(pin, 1);
#endif
}

static esp_err_t fn_init(void) {
    uint8_t v;
    esp_err_t err = sdmmc_io_read_byte(&s_card, 0, CCCR_FN_ENABLE, &v);
    if (err == ESP_OK) err = sdmmc_io_write_byte(&s_card, 0, CCCR_FN_ENABLE, v | BIT(1), NULL);
    for (int i = 0; err == ESP_OK; i++) {
        err = sdmmc_io_read_byte(&s_card, 0, CCCR_FN_READY, &v);
        if (err != ESP_OK || (v & BIT(1))) break;
        if (i == 10) return ESP_ERR_TIMEOUT;
        vTaskDelay(pdMS_TO_TICKS(10));
    }
    if (err == ESP_OK) err = sdmmc_io_read_byte(&s_card, 0, CCCR_INT_ENABLE, &v);
    if (err == ESP_OK) err = sdmmc_io_write_byte(&s_card, 0, CCCR_INT_ENABLE, v | BIT(0) | BIT(1), NULL);
    for (int fn = 0; fn <= 1 && err == ESP_OK; fn++) {
        err = sdmmc_io_write_byte(&s_card, 0, FBR_BLKSIZE(fn), HOSTED_SDIO_BLOCK & 0xff, NULL);
        if (err == ESP_OK) {
            err = sdmmc_io_write_byte(&s_card, 0, FBR_BLKSIZE(fn) + 1, HOSTED_SDIO_BLOCK >> 8, NULL);
        }
    }
    if (err == ESP_OK) err = sdmmc_io_enable_int(&s_card);
    return err;
}

esp_err_t hosted_sdio_connect(uint32_t timeout_ms) {
    reset_slave();
    sdmmc_host_t host = SDMMC_HOST_DEFAULT();
    host.slot = SLOT;
    host.max_freq_khz = CONFIG_ESP_HOSTED_ENHANCED_SDIO_FREQ_KHZ;
    host.flags |= SDMMC_HOST_FLAG_ALLOC_ALIGNED_BUF;

    TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(timeout_ms);
    for (;;) {
        memset(&s_card, 0, sizeof(s_card));
        if (sdmmc_card_init(&host, &s_card) == ESP_OK) {
            esp_err_t err = fn_init();
            if (err == ESP_OK) return ESP_OK;
            ESP_LOGD(TAG, "function init: %s", esp_err_to_name(err));
        }
        if ((int32_t)(deadline - xTaskGetTickCount()) <= 0) return ESP_ERR_TIMEOUT;
        vTaskDelay(pdMS_TO_TICKS(100));
    }
}

esp_err_t hosted_sdio_wait_int(TickType_t wait) {
    return sdmmc_io_wait_int(&s_card, wait);
}

esp_err_t hosted_sdio_read_int(uint32_t *int_raw, uint32_t *pkt_len) {
    uint32_t regs[(REG_PKT_LEN - REG_INT_RAW) / 4 + 1];
    xSemaphoreTake(s_reg_lock, portMAX_DELAY);
    esp_err_t err = sdmmc_io_read_bytes(&s_card, 1, REG_INT_RAW, regs, sizeof(regs));
    xSemaphoreGive(s_reg_lock);
    if (err != ESP_OK) return err;
    *int_raw = regs[0];
    *pkt_len = regs[(REG_PKT_LEN - REG_INT_RAW) / 4];
    return ESP_OK;
}

esp_err_t hosted_sdio_clear_int(uint32_t bits) {
    xSemaphoreTake(s_reg_lock, portMAX_DELAY);
    esp_err_t err = sdmmc_io_write_bytes(&s_card, 1, REG_INT_CLR, &bits, sizeof(bits));
    xSemaphoreGive(s_reg_lock);
    return err;
}

esp_err_t hosted_sdio_read_token(uint32_t *token) {
    xSemaphoreTake(s_reg_lock, portMAX_DELAY);
    esp_err_t err = sdmmc_io_read_bytes(&s_card, 1, REG_TOKEN_RDATA, token, sizeof(*token));
    xSemaphoreGive(s_reg_lock);
    return err;
}

esp_err_t hosted_sdio_notify(uint8_t bit) {
    return sdmmc_io_write_byte(&s_card, 1, REG_H2S_INT, BIT(bit), NULL);
}

esp_err_t hosted_sdio_read(void *buf, size_t len) {
    return sdmmc_io_read_blocks(&s_card, 1, DATA_END_ADDR - len, buf, hosted_sdio_padded(len));
}

esp_err_t hosted_sdio_write(const void *buf, size_t len) {
    return sdmmc_io_write_blocks(&s_card, 1, DATA_END_ADDR - len, buf, hosted_sdio_padded(len));
}
