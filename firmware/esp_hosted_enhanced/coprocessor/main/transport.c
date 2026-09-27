/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "transport.h"

#include <string.h>

#include "board.h"
#include "driver/sdio_slave.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "freertos/queue.h"
#include "freertos/task.h"

#define TX_BUF_NUM   20
#define RX_BUF_NUM   10
#define TASK_PRIO    21

#define FW_VERSION   ((2 << 16) | (12 << 8) | 6)
#define CHIP_ID_C6   0x0D

static const char *TAG = "transport";

static transport_cbs_t s_cbs;
static QueueHandle_t s_tx_free;
static TaskHandle_t s_ctrl_task;
static volatile bool s_open;

static void IRAM_ATTR h2s_event_cb(uint8_t pos) {
    BaseType_t woken = pdFALSE;
    if (pos == HOSTED_H2S_OPEN_DATA_PATH) {
        vTaskNotifyGiveFromISR(s_ctrl_task, &woken);
    } else if (pos == HOSTED_H2S_CLOSE_DATA_PATH) {
        s_open = false;
    }
    portYIELD_FROM_ISR(woken);
}

bool transport_is_open(void) {
    return s_open;
}

esp_err_t transport_send(hosted_if_t if_type, uint8_t flags, uint16_t seq,
                         const void *data, uint16_t len, TickType_t wait) {
    if (len > HOSTED_MAX_PAYLOAD) return ESP_ERR_INVALID_SIZE;
    uint8_t *buf;
    if (xQueueReceive(s_tx_free, &buf, wait) != pdTRUE) return ESP_ERR_TIMEOUT;

    hosted_header_t *h = (hosted_header_t *)buf;
    *h = (hosted_header_t){
        .if_type = if_type,
        .flags = flags,
        .len = len,
        .offset = sizeof(hosted_header_t),
        .seq_num = seq,
        .pkt_type = if_type == HOSTED_IF_PRIV ? HOSTED_PRIV_PKT_EVENT : 0,
    };
    memcpy(buf + sizeof(hosted_header_t), data, len);
    esp_err_t err = sdio_slave_send_queue(buf, sizeof(hosted_header_t) + len, buf, portMAX_DELAY);
    if (err != ESP_OK) xQueueSend(s_tx_free, &buf, 0);
    return err;
}

static void send_init_event(void) {
    uint8_t ev[64];
    size_t n = 0;
    ev[n++] = HOSTED_PRIV_EVENT_INIT;
    n++;
    const uint8_t tlv[][3] = {
        {HOSTED_TLV_CHIP_ID, 1, CHIP_ID_C6},
        {HOSTED_TLV_SDIO_MODE, 1, 1},
        {HOSTED_TLV_CAPABILITY, 1, HOSTED_CAP_WLAN_SDIO},
        {HOSTED_TLV_TEST_RAW_TP, 1, 0},
        {HOSTED_TLV_TX_Q_SIZE, 1, TX_BUF_NUM},
        {HOSTED_TLV_RX_Q_SIZE, 1, RX_BUF_NUM},
    };
    for (size_t i = 0; i < sizeof(tlv) / sizeof(tlv[0]); i++) {
        memcpy(&ev[n], tlv[i], 3);
        n += 3;
    }
    uint32_t ver = FW_VERSION;
    ev[n++] = HOSTED_TLV_FW_VERSION;
    ev[n++] = 4;
    memcpy(&ev[n], &ver, 4);
    n += 4;
    ev[n++] = HOSTED_TLV_EXT_CAPS;
    ev[n++] = 4;
    memcpy(&ev[n], &s_cbs.ext_caps, 4);
    n += 4;
    ev[1] = n - 2;
    transport_send(HOSTED_IF_PRIV, 0, 0, ev, n, portMAX_DELAY);
}

static void on_priv(const uint8_t *p, uint16_t len) {
    if (len < 2 || p[0] != HOSTED_PRIV_EVENT_EXT_CTRL || p[1] > len - 2) return;
    for (size_t pos = 2; pos + 2 <= 2u + p[1] && pos + 2 + p[pos + 1] <= 2u + p[1];
         pos += 2 + p[pos + 1]) {
        if (p[pos] == HOSTED_TLV_CTRL_LOG && p[pos + 1] == 1 && s_cbs.on_log_enable) {
            s_cbs.on_log_enable(p[pos + 2]);
        }
    }
}

static void ctrl_task(void *arg) {
    for (;;) {
        ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
        ESP_LOGI(TAG, "data path open");
        s_open = true;
        send_init_event();
        if (s_cbs.on_open) s_cbs.on_open();
    }
}

static void tx_done_task(void *arg) {
    for (;;) {
        void *buf;
        if (sdio_slave_send_get_finished(&buf, portMAX_DELAY) == ESP_OK) {
            xQueueSend(s_tx_free, &buf, 0);
        }
    }
}

static void rx_task(void *arg) {
    for (;;) {
        sdio_slave_buf_handle_t handle;
        uint8_t *buf;
        size_t size;
        if (sdio_slave_recv(&handle, &buf, &size, portMAX_DELAY) != ESP_OK) continue;

        for (size_t pos = 0; size - pos >= sizeof(hosted_header_t);) {
            const hosted_header_t *h = (const hosted_header_t *)(buf + pos);
            uint16_t len = h->len;
            if (h->offset != sizeof(hosted_header_t) || len == 0 ||
                sizeof(hosted_header_t) + len > size - pos) {
                break;
            }
            uint8_t *payload = buf + pos + sizeof(hosted_header_t);
            switch (h->if_type) {
            case HOSTED_IF_STA:
                if (s_cbs.on_sta) s_cbs.on_sta(payload, len);
                break;
            case HOSTED_IF_SERIAL:
                if (s_cbs.on_serial) s_cbs.on_serial(payload, len, h->flags);
                break;
            case HOSTED_IF_TEST:
                if (s_cbs.on_test) s_cbs.on_test(payload, len);
                break;
            case HOSTED_IF_PRIV:
                on_priv(payload, len);
                break;
            default:
                break;
            }
            pos += sizeof(hosted_header_t) + len;
        }
        sdio_slave_recv_load_buf(handle);
    }
}

esp_err_t transport_init(const transport_cbs_t *cbs) {
    s_cbs = *cbs;

    s_tx_free = xQueueCreate(TX_BUF_NUM, sizeof(uint8_t *));
    for (int i = 0; i < TX_BUF_NUM; i++) {
        uint8_t *buf = heap_caps_aligned_alloc(4, HOSTED_BUF_SIZE, MALLOC_CAP_DMA);
        if (!buf) return ESP_ERR_NO_MEM;
        xQueueSend(s_tx_free, &buf, 0);
    }

    xTaskCreate(ctrl_task, "h_ctrl", 3072, NULL, TASK_PRIO, &s_ctrl_task);

    sdio_slave_config_t config = {
        .timing = board_sdio_timing(),
        .sending_mode = SDIO_SLAVE_SEND_STREAM,
        .send_queue_size = TX_BUF_NUM,
        .recv_buffer_size = HOSTED_BUF_SIZE,
        .event_cb = h2s_event_cb,
        .flags = board_sdio_flags(),
    };
    esp_err_t err = sdio_slave_initialize(&config);
    if (err != ESP_OK) return err;
    board_sdio_pins_ready();

    for (int i = 0; i < RX_BUF_NUM; i++) {
        uint8_t *buf = heap_caps_aligned_alloc(4, HOSTED_BUF_SIZE, MALLOC_CAP_DMA);
        if (!buf) return ESP_ERR_NO_MEM;
        sdio_slave_buf_handle_t handle = sdio_slave_recv_register_buf(buf);
        if (!handle) return ESP_FAIL;
        sdio_slave_recv_load_buf(handle);
    }

    sdio_slave_set_host_intena(SDIO_SLAVE_HOSTINT_SEND_NEW_PACKET |
                               SDIO_SLAVE_HOSTINT_BIT0 | SDIO_SLAVE_HOSTINT_BIT1 |
                               SDIO_SLAVE_HOSTINT_BIT2 | SDIO_SLAVE_HOSTINT_BIT3 |
                               SDIO_SLAVE_HOSTINT_BIT4 | SDIO_SLAVE_HOSTINT_BIT5 |
                               SDIO_SLAVE_HOSTINT_BIT6 | SDIO_SLAVE_HOSTINT_BIT7);

    xTaskCreate(tx_done_task, "h_txdone", 2048, NULL, TASK_PRIO + 1, NULL);
    xTaskCreate(rx_task, "h_rx", 4096, NULL, TASK_PRIO, NULL);
    return sdio_slave_start();
}
