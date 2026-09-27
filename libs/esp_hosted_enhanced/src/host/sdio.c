/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The IDF sdmmc driver brings the card up and waits for DAT1; every command
 * after that is issued here directly and polled.
 */

#include "host/sdio.h"

#include <string.h>

#include "driver/gpio.h"
#include "driver/sdmmc_host.h"
#include "esp_cache.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "hal/sdmmc_ll.h"
#include "hosted_host.h"
#include "sdkconfig.h"
#include "sd_protocol_defs.h"
#include "sdmmc_cmd.h"
#include "soc/sdmmc_struct.h"

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
#define FUNC              1

#define EV_ERR     (SDMMC_LL_EVENT_RESP_ERR | SDMMC_LL_EVENT_RCRC | SDMMC_LL_EVENT_DCRC | \
                    SDMMC_LL_EVENT_RTO | SDMMC_LL_EVENT_DTO | SDMMC_LL_EVENT_HTO | \
                    SDMMC_LL_EVENT_SBE | SDMMC_LL_EVENT_EBE)
#define EV_DONE    (SDMMC_LL_EVENT_CMD_DONE | SDMMC_LL_EVENT_DATA_OVER)
#define IDMAC_INTS (SDMMC_IDMAC_INTMASK_NI | SDMMC_IDMAC_INTMASK_RI | SDMMC_IDMAC_INTMASK_TI)
#define R5_ERRORS  0xcb
#define DESC_NUM   10
#define CACHE_LINE 64
#define SMALL_LEN  CACHE_LINE
#define TIMEOUT_US 20000

static const char *TAG = "hosted_sdio";

static sdmmc_card_t s_card;
static StaticSemaphore_t s_lock_buf;
static SemaphoreHandle_t s_lock;
static sdmmc_desc_t *s_desc;
static uint8_t *s_small;
static bool s_polled;
static bool s_card_ready;
static int s_idf_depth;
static uint32_t s_clock_div;

static SemaphoreHandle_t lock(void) {
    if (!s_lock) {
        static portMUX_TYPE mux = portMUX_INITIALIZER_UNLOCKED;
        portENTER_CRITICAL(&mux);
        if (!s_lock) s_lock = xSemaphoreCreateRecursiveMutexStatic(&s_lock_buf);
        portEXIT_CRITICAL(&mux);
    }
    return s_lock;
}

static void take(void) {
    xSemaphoreTakeRecursive(lock(), portMAX_DELAY);
}

static void give(void) {
    xSemaphoreGiveRecursive(lock());
}

/* The IDF ISR would consume the completion events polled here, and would
 * report the FIFO request bits the polled transfers leave raised. */
static void set_polled(bool polled) {
    if (!polled) {
        sdmmc_ll_clear_interrupt(&SDMMC, SDMMC_LL_SD_EVENT_MASK);
        sdmmc_ll_clear_idsts_interrupt(&SDMMC, sdmmc_ll_get_idsts_interrupt_raw(&SDMMC));
    }
    sdmmc_ll_enable_interrupt(&SDMMC, SDMMC_LL_SD_EVENT_MASK, !polled);
    SDMMC.idinten.val = polled ? 0 : IDMAC_INTS;
    s_polled = polled;
}

void hosted_host_sdmmc_acquire(void) {
    take();
    if (s_idf_depth++ == 0 && s_polled) set_polled(false);
}

/* The host clock divider is shared by both slots and the IDF driver reprograms
 * it per transaction for the slot it talks to; one IDF command here puts ours
 * back before polling resumes. */
void hosted_host_sdmmc_release(void) {
    if (s_idf_depth == 1 && s_card_ready && sdmmc_ll_get_clock_div(&SDMMC) != s_clock_div) {
        uint8_t v;
        sdmmc_io_read_byte(&s_card, 0, CCCR_FN_ENABLE, &v);
    }
    if (--s_idf_depth == 0 && s_card_ready) set_polled(true);
    give();
}

esp_err_t hosted_host_sdmmc_init(void) {
    hosted_host_sdmmc_acquire();
    esp_err_t err = sdmmc_host_init();
    hosted_host_sdmmc_release();
    return err == ESP_ERR_INVALID_STATE ? ESP_OK : err;
}

esp_err_t hosted_host_sdmmc_do_transaction(int slot, sdmmc_command_t *cmd) {
    hosted_host_sdmmc_acquire();
    esp_err_t err = sdmmc_host_do_transaction(slot, cmd);
    hosted_host_sdmmc_release();
    return err;
}

static void recover(void) {
    sdmmc_ll_reset_fifo(&SDMMC);
    sdmmc_ll_reset_dma(&SDMMC);
    int64_t t0 = esp_timer_get_time();
    while ((!sdmmc_ll_is_fifo_reset_done(&SDMMC) || !sdmmc_ll_is_dma_reset_done(&SDMMC)) &&
           esp_timer_get_time() - t0 < TIMEOUT_US) {
    }
    sdmmc_ll_clear_interrupt(&SDMMC, EV_ERR | EV_DONE);
    sdmmc_ll_clear_idsts_interrupt(&SDMMC, sdmmc_ll_get_idsts_interrupt_raw(&SDMMC));
}

static esp_err_t issue(sdmmc_hw_cmd_t cmd, uint32_t arg, uint32_t done) {
    int64_t t0 = esp_timer_get_time();
    while (!sdmmc_ll_is_command_taken(&SDMMC)) {
        if (esp_timer_get_time() - t0 > TIMEOUT_US) return ESP_ERR_TIMEOUT;
    }
    cmd.card_num = SLOT;
    cmd.use_hold_reg = 1;
    cmd.wait_complete = 1;
    cmd.response_expect = 1;
    cmd.check_response_crc = 1;
    cmd.start_command = 1;
    sdmmc_ll_set_command_arg(&SDMMC, arg);
    sdmmc_ll_set_command(&SDMMC, cmd);

    esp_err_t err = ESP_OK;
    for (;;) {
        uint32_t st = sdmmc_ll_get_interrupt_raw(&SDMMC);
        if (st & EV_ERR) {
            err = (st & (SDMMC_LL_EVENT_RCRC | SDMMC_LL_EVENT_DCRC)) ? ESP_ERR_INVALID_CRC : ESP_ERR_TIMEOUT;
            break;
        }
        if ((st & done) == done) {
            sdmmc_ll_clear_interrupt(&SDMMC, done);
            break;
        }
        if (esp_timer_get_time() - t0 > TIMEOUT_US) {
            err = ESP_ERR_TIMEOUT;
            break;
        }
    }
    if (err == ESP_OK && ((SDMMC.resp[0] >> 8) & R5_ERRORS)) err = ESP_ERR_INVALID_RESPONSE;
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "CMD%d %08lx: %s", cmd.cmd_index, (unsigned long)arg, esp_err_to_name(err));
        recover();
    }
    return err;
}

static esp_err_t cmd52(bool write, uint32_t addr, uint8_t in, uint8_t *out) {
    sdmmc_hw_cmd_t cmd = {0};
    cmd.cmd_index = SD_IO_RW_DIRECT;
    uint32_t arg = (write ? SD_ARG_CMD52_WRITE : 0) | (FUNC << SD_ARG_CMD52_FUNC_SHIFT) |
                   ((addr & SD_ARG_CMD52_REG_MASK) << SD_ARG_CMD52_REG_SHIFT) | in;
    esp_err_t err = issue(cmd, arg, SDMMC_LL_EVENT_CMD_DONE);
    if (err == ESP_OK && out) *out = SDMMC.resp[0] & 0xff;
    return err;
}

/* `buf` must be cache-line aligned and sized; block mode when `len` is a whole
 * number of blocks. */
static esp_err_t cmd53(bool write, uint32_t addr, void *buf, size_t len) {
    size_t n = (len + SDMMC_DMA_MAX_BUF_LEN - 1) / SDMMC_DMA_MAX_BUF_LEN;
    if (n > DESC_NUM) return ESP_ERR_INVALID_SIZE;
    bool block = len % HOSTED_SDIO_BLOCK == 0;
    size_t sync_len = (len + CACHE_LINE - 1) & ~(size_t)(CACHE_LINE - 1);
    esp_cache_msync(buf, sync_len, ESP_CACHE_MSYNC_FLAG_DIR_C2M);

    uint8_t *p = buf;
    for (size_t i = 0; i < n; i++) {
        size_t chunk = len - i * SDMMC_DMA_MAX_BUF_LEN;
        if (chunk > SDMMC_DMA_MAX_BUF_LEN) chunk = SDMMC_DMA_MAX_BUF_LEN;
        sdmmc_desc_t *d = &s_desc[i];
        memset(d, 0, sizeof(*d));
        d->first_descriptor = i == 0;
        d->last_descriptor = i == n - 1;
        d->second_address_chained = 1;
        d->owned_by_idmac = 1;
        d->buffer1_size = chunk;
        d->buffer1_ptr = p;
        d->next_desc_ptr = i == n - 1 ? NULL : &s_desc[i + 1];
        p += chunk;
    }
    esp_cache_msync(s_desc, n * sizeof(sdmmc_desc_t), ESP_CACHE_MSYNC_FLAG_DIR_C2M);
    sdmmc_ll_set_data_transfer_len(&SDMMC, len);
    sdmmc_ll_set_block_size(&SDMMC, block ? HOSTED_SDIO_BLOCK : len);
    sdmmc_ll_set_desc_addr(&SDMMC, (uint32_t)s_desc);
    sdmmc_ll_enable_dma(&SDMMC, true);
    sdmmc_ll_poll_demand(&SDMMC);

    sdmmc_hw_cmd_t cmd = {0};
    cmd.cmd_index = SD_IO_RW_EXTENDED;
    cmd.data_expected = 1;
    cmd.rw = write;
    uint32_t count = block ? len / HOSTED_SDIO_BLOCK : len % HOSTED_SDIO_BLOCK;
    uint32_t arg = (write ? SD_ARG_CMD53_WRITE : 0) | (FUNC << SD_ARG_CMD53_FUNC_SHIFT) |
                   (block ? SD_ARG_CMD53_BLOCK_MODE : 0) | SD_ARG_CMD53_INCREMENT |
                   ((addr & SD_ARG_CMD53_REG_MASK) << SD_ARG_CMD53_REG_SHIFT) |
                   (count & SD_ARG_CMD53_LENGTH_MASK);
    esp_err_t err = issue(cmd, arg, EV_DONE);
    sdmmc_ll_clear_idsts_interrupt(&SDMMC, sdmmc_ll_get_idsts_interrupt_raw(&SDMMC));
    if (!write) esp_cache_msync(buf, sync_len, ESP_CACHE_MSYNC_FLAG_DIR_M2C);
    return err;
}

esp_err_t hosted_sdio_init(void) {
    s_desc = heap_caps_aligned_calloc(64, DESC_NUM, sizeof(sdmmc_desc_t), MALLOC_CAP_INTERNAL | MALLOC_CAP_DMA);
    s_small = heap_caps_aligned_alloc(64, SMALL_LEN, MALLOC_CAP_INTERNAL | MALLOC_CAP_DMA);
    if (!s_desc || !s_small) return ESP_ERR_NO_MEM;

    esp_err_t err = hosted_host_sdmmc_init();
    if (err != ESP_OK) return err;
    sdmmc_slot_config_t slot = SDMMC_SLOT_CONFIG_DEFAULT();
    slot.width = 4;
    slot.clk = CONFIG_ESP_HOSTED_ENHANCED_PIN_CLK;
    slot.cmd = CONFIG_ESP_HOSTED_ENHANCED_PIN_CMD;
    slot.d0 = CONFIG_ESP_HOSTED_ENHANCED_PIN_D0;
    slot.d1 = CONFIG_ESP_HOSTED_ENHANCED_PIN_D1;
    slot.d2 = CONFIG_ESP_HOSTED_ENHANCED_PIN_D2;
    slot.d3 = CONFIG_ESP_HOSTED_ENHANCED_PIN_D3;
    slot.flags |= SDMMC_SLOT_FLAG_INTERNAL_PULLUP;
    hosted_host_sdmmc_acquire();
    err = sdmmc_host_init_slot(SLOT, &slot);
    hosted_host_sdmmc_release();
    return err;
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
    if (err == ESP_OK) err = sdmmc_io_write_byte(&s_card, 0, CCCR_FN_ENABLE, v | BIT(FUNC), NULL);
    for (int i = 0; err == ESP_OK; i++) {
        err = sdmmc_io_read_byte(&s_card, 0, CCCR_FN_READY, &v);
        if (err != ESP_OK || (v & BIT(FUNC))) break;
        if (i == 10) return ESP_ERR_TIMEOUT;
        vTaskDelay(pdMS_TO_TICKS(10));
    }
    if (err == ESP_OK) err = sdmmc_io_read_byte(&s_card, 0, CCCR_INT_ENABLE, &v);
    if (err == ESP_OK) err = sdmmc_io_write_byte(&s_card, 0, CCCR_INT_ENABLE, v | BIT(0) | BIT(FUNC), NULL);
    for (int fn = 0; fn <= FUNC && err == ESP_OK; fn++) {
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
        hosted_host_sdmmc_acquire();
        s_card_ready = false;
        memset(&s_card, 0, sizeof(s_card));
        esp_err_t err = sdmmc_card_init(&host, &s_card);
        if (err == ESP_OK) err = fn_init();
        s_clock_div = sdmmc_ll_get_clock_div(&SDMMC);
        s_card_ready = err == ESP_OK;
        hosted_host_sdmmc_release();
        if (err == ESP_OK) return ESP_OK;
        ESP_LOGD(TAG, "card init: %s", esp_err_to_name(err));
        if ((int32_t)(deadline - xTaskGetTickCount()) <= 0) return ESP_ERR_TIMEOUT;
        vTaskDelay(pdMS_TO_TICKS(100));
    }
}

esp_err_t hosted_sdio_wait_int(TickType_t wait) {
    return sdmmc_io_wait_int(&s_card, wait);
}

esp_err_t hosted_sdio_read_status(hosted_sdio_status_t *st) {
    take();
    esp_err_t err = cmd53(false, REG_TOKEN_RDATA, s_small, REG_PKT_LEN - REG_TOKEN_RDATA + 4);
    const uint32_t *r = (const uint32_t *)s_small;
    st->token = r[0];
    st->int_raw = r[(REG_INT_RAW - REG_TOKEN_RDATA) / 4];
    st->pkt_len = r[(REG_PKT_LEN - REG_TOKEN_RDATA) / 4];
    give();
    return err;
}

esp_err_t hosted_sdio_clear_int(uint32_t bits) {
    esp_err_t err = ESP_OK;
    take();
    for (int i = 0; i < 4 && err == ESP_OK; i++) {
        uint8_t b = bits >> (8 * i);
        if (b) err = cmd52(true, REG_INT_CLR + i, b, NULL);
    }
    give();
    return err;
}

esp_err_t hosted_sdio_read_token(uint32_t *token) {
    take();
    esp_err_t err = cmd53(false, REG_TOKEN_RDATA, s_small, sizeof(*token));
    *token = *(const uint32_t *)s_small;
    give();
    return err;
}

esp_err_t hosted_sdio_notify(uint8_t bit) {
    take();
    esp_err_t err = cmd52(true, REG_H2S_INT, BIT(bit), NULL);
    give();
    return err;
}

esp_err_t hosted_sdio_read(void *buf, size_t len) {
    take();
    esp_err_t err = cmd53(false, DATA_END_ADDR - len, buf, hosted_sdio_padded(len));
    give();
    return err;
}

esp_err_t hosted_sdio_write(const void *buf, size_t len) {
    take();
    esp_err_t err = cmd53(true, DATA_END_ADDR - len, (void *)buf, hosted_sdio_padded(len));
    give();
    return err;
}
