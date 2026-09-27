/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "host/transport.h"

#include <stdio.h>
#include <string.h>

#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/event_groups.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "host/sdio.h"

#define TX_BUF_NUM         8
#define RX_STREAM_MAX      (32 * 1024)
#define TASK_PRIO          22
#define TOKEN_POLL_US      300
#define INIT_TIMEOUT_MS    5000
#define CONNECT_TIMEOUT_MS 4000
#define START_ATTEMPTS     3

#define S2H_NEW_PACKET     BIT(23)
#define PKT_LEN_MASK       0xFFFFF
#define TOKEN_MASK         0xFFF
#define CHIP_ID_C6         0x0D

#define EVT_READY          BIT0

#define LOG_PREFIX         "[C6] "
#define LOG_PREFIX_LEN     (sizeof(LOG_PREFIX) - 1)

typedef struct {
    uint8_t *buf;
    size_t len;
} tx_item_t;

static const char *TAG = "hosted";

static QueueHandle_t s_tx_free;
static QueueHandle_t s_tx_queue;
static EventGroupHandle_t s_events;
static TaskHandle_t s_tx_task;
static esp_timer_handle_t s_poll_timer;
static uint8_t *s_rx_buf;
static uint32_t s_rx_bytes;
static uint32_t s_tx_count;
static _Atomic uint32_t s_tokens;
static bool s_started;
static TaskHandle_t s_rx_task;
static uint32_t s_ext_caps;
static hosted_transport_rx_cb_t s_test_rx;
static void *s_test_rx_arg;
static char s_log_line[256] = LOG_PREFIX;
static size_t s_log_len = LOG_PREFIX_LEN;

/* SDMMC DMA out of PSRAM costs about half again per block written, so the
 * transmit side stays internal; the receive side barely notices. */
static void *tx_alloc(size_t size) {
    return heap_caps_aligned_alloc(64, size, MALLOC_CAP_INTERNAL | MALLOC_CAP_DMA);
}

static void *rx_alloc(size_t size) {
    return heap_caps_malloc(size, MALLOC_CAP_SPIRAM | MALLOC_CAP_CACHE_ALIGNED);
}

uint32_t hosted_transport_ext_caps(void) {
    return s_ext_caps;
}

void hosted_transport_set_test_rx(hosted_transport_rx_cb_t cb, void *arg) {
    s_test_rx_arg = arg;
    s_test_rx = cb;
}

bool hosted_transport_ready(void) {
    return s_events && (xEventGroupGetBits(s_events) & EVT_READY);
}

esp_err_t hosted_transport_send(hosted_if_t if_type, uint8_t flags,
                                const void *data, size_t len, TickType_t wait) {
    if (len > HOSTED_MAX_PAYLOAD) return ESP_ERR_INVALID_SIZE;
    if (!hosted_transport_ready() && if_type != HOSTED_IF_PRIV) return ESP_ERR_INVALID_STATE;
    uint8_t *buf;
    if (xQueueReceive(s_tx_free, &buf, wait) != pdTRUE) return ESP_ERR_TIMEOUT;
    *(hosted_header_t *)buf = (hosted_header_t){
        .if_type = if_type,
        .flags = flags,
        .len = len,
        .offset = sizeof(hosted_header_t),
    };
    memcpy(buf + sizeof(hosted_header_t), data, len);
    tx_item_t item = {buf, sizeof(hosted_header_t) + len};
    xQueueSend(s_tx_queue, &item, portMAX_DELAY);
    return ESP_OK;
}

static void poll_timer_cb(void *arg) {
    xTaskNotifyGive(s_tx_task);
}

/* The slave raises no interrupt when it returns receive buffers, so a short
 * timer paces the token polls instead of a tick-granular delay. */
static void wait_tokens(uint32_t need) {
    for (;;) {
        uint32_t avail = (s_tokens - s_tx_count) & TOKEN_MASK;
        if (avail >= need) return;
        uint32_t reg;
        if (hosted_sdio_read_token(&reg) == ESP_OK) {
            s_tokens = (reg >> 16) & TOKEN_MASK;
            if (((s_tokens - s_tx_count) & TOKEN_MASK) >= need) return;
        }
        esp_timer_start_once(s_poll_timer, TOKEN_POLL_US);
        ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
    }
}

static void coalesce(tx_item_t *item) {
    tx_item_t next;
    while (xQueuePeek(s_tx_queue, &next, 0) == pdTRUE && item->len + next.len <= HOSTED_BUF_SIZE) {
        xQueueReceive(s_tx_queue, &next, 0);
        memcpy(item->buf + item->len, next.buf, next.len);
        item->len += next.len;
        xQueueSend(s_tx_free, &next.buf, 0);
    }
}

static void tx_task(void *arg) {
    for (;;) {
        tx_item_t item;
        xQueueReceive(s_tx_queue, &item, portMAX_DELAY);
        if (s_ext_caps & HOSTED_EXT_CAP_RX_AGGR) coalesce(&item);
        uint32_t need = (item.len + HOSTED_BUF_SIZE - 1) / HOSTED_BUF_SIZE;
        wait_tokens(need);
        esp_err_t err = hosted_sdio_write(item.buf, item.len);
        if (err == ESP_OK) {
            s_tx_count = (s_tx_count + need) & TOKEN_MASK;
        } else {
            ESP_LOGE(TAG, "write %u: %s", (unsigned)item.len, esp_err_to_name(err));
        }
        xQueueSend(s_tx_free, &item.buf, 0);
    }
}

static void send_slave_config(void) {
    const uint8_t ev[] = {
        HOSTED_PRIV_EVENT_INIT, 15,
        HOSTED_TLV_HOST_CAPABILITIES, 1, 0,
        HOSTED_TLV_RCVD_CHIP_ID, 1, CHIP_ID_C6,
        HOSTED_TLV_SLV_TEST_RAW_TP, 1, 0,
        HOSTED_TLV_THROTTLE_HIGH, 1, 80,
        HOSTED_TLV_THROTTLE_LOW, 1, 60,
    };
    hosted_transport_send(HOSTED_IF_PRIV, 0, ev, sizeof(ev), portMAX_DELAY);
}

static void on_priv(const uint8_t *p, uint16_t len) {
    if (len < 2 || p[0] != HOSTED_PRIV_EVENT_INIT || p[1] > len - 2) return;
    int chip = -1;
    int caps = -1;
    uint32_t version = 0;
    uint32_t ext = 0;
    for (size_t pos = 2; pos + 2 <= 2u + p[1];) {
        uint8_t tag = p[pos], n = p[pos + 1];
        const uint8_t *v = &p[pos + 2];
        if (pos + 2 + n > 2u + p[1]) break;
        if (tag == HOSTED_TLV_CHIP_ID && n == 1) chip = v[0];
        if (tag == HOSTED_TLV_CAPABILITY && n == 1) caps = v[0];
        if (tag == HOSTED_TLV_EXT_CAPS && n == 4) ext = v[0] | v[1] << 8 | v[2] << 16 | (uint32_t)v[3] << 24;
        if (tag == HOSTED_TLV_FW_VERSION && n == 4) version = v[0] | v[1] << 8 | v[2] << 16;
        pos += 2 + n;
    }
    ESP_LOGI(TAG, "coprocessor chip 0x%02x, caps 0x%02x, ext 0x%02x, firmware %u.%u.%u", chip, caps,
             (unsigned)ext, (unsigned)(version >> 16), (unsigned)((version >> 8) & 0xff),
             (unsigned)(version & 0xff));
    s_ext_caps = ext;
    if (chip != CHIP_ID_C6) {
        ESP_LOGE(TAG, "unsupported coprocessor chip");
        return;
    }
    send_slave_config();
    xEventGroupSetBits(s_events, EVT_READY);
}

/* Whole lines only: a partial one left on the console would swallow the next
 * line the host itself prints. */
static void print_log(const uint8_t *p, size_t len) {
    for (size_t i = 0; i < len; i++) {
        if (s_log_len < sizeof(s_log_line) - 1) s_log_line[s_log_len++] = p[i];
        if (p[i] == '\n' || s_log_len == sizeof(s_log_line) - 1) {
            if (p[i] != '\n') s_log_line[s_log_len++] = '\n';
            fwrite(s_log_line, 1, s_log_len, stdout);
            fflush(stdout);
            s_log_len = LOG_PREFIX_LEN;
        }
    }
}

static void dispatch_stream(const uint8_t *p, size_t len) {
    while (len >= sizeof(hosted_header_t)) {
        const hosted_header_t *h = (const hosted_header_t *)p;
        size_t total = h->offset + h->len;
        if (h->offset != sizeof(hosted_header_t) || h->len == 0 || h->len > HOSTED_MAX_PAYLOAD ||
            total > len) {
            ESP_LOGW(TAG, "malformed stream, %u bytes dropped", (unsigned)len);
            return;
        }
        const uint8_t *payload = p + h->offset;
        switch (h->if_type) {
        case HOSTED_IF_STA:
            hosted_wifi_on_sta(payload, h->len);
            break;
        case HOSTED_IF_SERIAL:
            hosted_rpc_on_serial(payload, h->len, h->flags);
            break;
        case HOSTED_IF_PRIV:
            on_priv(payload, h->len);
            break;
        case HOSTED_IF_TEST:
            if (s_test_rx) s_test_rx(payload, h->len, s_test_rx_arg);
            break;
        case HOSTED_IF_LOG:
            print_log(payload, h->len);
            break;
        default:
            break;
        }
        p += total;
        len -= total;
    }
}

static void rx_task(void *arg) {
    for (;;) {
        if (hosted_sdio_wait_int(portMAX_DELAY) != ESP_OK) continue;
        hosted_sdio_status_t st;
        if (hosted_sdio_read_status(&st) != ESP_OK) {
            ESP_LOGE(TAG, "interrupt status read failed");
            continue;
        }
        s_tokens = (st.token >> 16) & TOKEN_MASK;
        uint32_t raw = st.int_raw, pkt_len = st.pkt_len;
        hosted_sdio_clear_int(raw);
        if (!(raw & S2H_NEW_PACKET)) continue;

        size_t len = ((pkt_len & PKT_LEN_MASK) - s_rx_bytes) & PKT_LEN_MASK;
        if (len == 0) continue;
        if (len > RX_STREAM_MAX) {
            ESP_LOGE(TAG, "stream of %u bytes exceeds the buffer", (unsigned)len);
            s_rx_bytes = pkt_len & PKT_LEN_MASK;
            continue;
        }
        esp_err_t err = hosted_sdio_read(s_rx_buf, len);
        s_rx_bytes = (s_rx_bytes + len) & PKT_LEN_MASK;
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "read %u: %s", (unsigned)len, esp_err_to_name(err));
            continue;
        }
        dispatch_stream(s_rx_buf, len);
    }
}

static esp_err_t create(void) {
    s_events = xEventGroupCreate();
    s_tx_free = xQueueCreate(TX_BUF_NUM, sizeof(uint8_t *));
    s_tx_queue = xQueueCreate(TX_BUF_NUM, sizeof(tx_item_t));
    s_rx_buf = rx_alloc(hosted_sdio_padded(RX_STREAM_MAX));
    if (!s_events || !s_tx_free || !s_tx_queue || !s_rx_buf) return ESP_ERR_NO_MEM;
    for (int i = 0; i < TX_BUF_NUM; i++) {
        uint8_t *buf = tx_alloc(hosted_sdio_padded(HOSTED_BUF_SIZE));
        if (!buf) return ESP_ERR_NO_MEM;
        xQueueSend(s_tx_free, &buf, 0);
    }
    xTaskCreate(tx_task, "hosted_tx", 3072, NULL, TASK_PRIO, &s_tx_task);
    const esp_timer_create_args_t timer = {.callback = poll_timer_cb, .name = "hosted_poll"};
    esp_err_t err = esp_timer_create(&timer, &s_poll_timer);
    if (err != ESP_OK) return err;
    return hosted_sdio_init();
}

esp_err_t hosted_transport_start(void) {
    if (!s_started) {
        esp_err_t err = create();
        if (err != ESP_OK) return err;
        s_started = true;
    }
    if (hosted_transport_ready()) return ESP_OK;

    for (int attempt = 0; attempt < START_ATTEMPTS; attempt++) {
        s_rx_bytes = 0;
        s_tx_count = 0;
        s_tokens = 0;
        s_ext_caps = 0;
        esp_err_t err = hosted_sdio_connect(CONNECT_TIMEOUT_MS);
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "coprocessor not responding: %s", esp_err_to_name(err));
            continue;
        }
        if (!s_rx_task) xTaskCreate(rx_task, "hosted_rx", 4096, NULL, TASK_PRIO, &s_rx_task);
        hosted_sdio_notify(HOSTED_H2S_OPEN_DATA_PATH);
        if (xEventGroupWaitBits(s_events, EVT_READY, pdFALSE, pdTRUE,
                                pdMS_TO_TICKS(INIT_TIMEOUT_MS)) & EVT_READY) {
            return ESP_OK;
        }
        ESP_LOGW(TAG, "no INIT event from the coprocessor");
    }
    return ESP_ERR_TIMEOUT;
}
