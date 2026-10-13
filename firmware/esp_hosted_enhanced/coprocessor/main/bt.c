/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "bt.h"

#include <string.h>

#include "esp_bt.h"
#include "esp_hci_internal.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "os/os_mbuf.h"
#include "transport.h"

#define TX_WAIT_MS      100
#define ACL_RETRY_MS    100

static const char *TAG = "bt";

static volatile bool s_enabled;

static void send_to_host(uint8_t type, const uint8_t *data, struct os_mbuf *om, uint16_t len) {
    uint8_t *p = NULL;
    if (s_enabled && transport_is_open() && len + 1 <= HOSTED_MAX_PAYLOAD) {
        p = transport_tx_get(HOSTED_IF_HCI, pdMS_TO_TICKS(TX_WAIT_MS));
    }
    if (!p) {
        ESP_LOGW(TAG, "hci type %u len %u to host dropped", type, len);
        return;
    }
    p[0] = type;
    if (om) {
        os_mbuf_copydata(om, 0, len, p + 1);
    } else {
        memcpy(p + 1, data, len);
    }
    transport_tx_put(p, HOSTED_IF_HCI, 0, 0, len + 1);
}

static int on_controller_evt(uint8_t *ev, void *arg) {
    send_to_host(HOSTED_HCI_EVT, ev, NULL, ev[1] + 2);
    r_ble_hci_trans_buf_free(ev);
    return 0;
}

static int on_controller_acl(struct os_mbuf *om, void *arg) {
    send_to_host(HOSTED_HCI_ACL, NULL, om, OS_MBUF_PKTLEN(om));
    os_mbuf_free_chain(om);
    return 0;
}

/* Holding the SDIO buffer while the controller is out of ACL buffers stalls
 * the host through the token count instead of dropping the packet. */
static void acl_to_controller(const uint8_t *payload, uint16_t len) {
    TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(ACL_RETRY_MS);
    struct os_mbuf *om;
    while (!(om = os_msys_get_pkthdr(len, ESP_HCI_INTERNAL_ACL_MBUF_LEADINGSPCAE))) {
        if (!s_enabled || (int32_t)(deadline - xTaskGetTickCount()) <= 0) {
            ESP_LOGW(TAG, "no controller buffer, acl len %u dropped", len);
            return;
        }
        vTaskDelay(1);
    }
    if (os_mbuf_append(om, payload, len) != 0) {
        os_mbuf_free_chain(om);
        ESP_LOGW(TAG, "acl len %u append failed", len);
        return;
    }
    r_ble_hci_trans_hs_acl_tx(om);
}

void bt_on_host_packet(uint8_t type, const uint8_t *payload, uint16_t len) {
    if (!s_enabled) return;
    if (type == HOSTED_HCI_CMD && len >= 3 && len == 3 + payload[2]) {
        uint8_t *cmd = r_ble_hci_trans_buf_alloc(ESP_HCI_INTERNAL_BUF_CMD);
        if (!cmd) {
            ESP_LOGW(TAG, "no controller buffer, cmd 0x%04x dropped", payload[0] | payload[1] << 8);
            return;
        }
        memcpy(cmd, payload, len);
        r_ble_hci_trans_hs_cmd_tx(cmd);
    } else if (type == HOSTED_HCI_ACL && len >= 4) {
        acl_to_controller(payload, len);
    } else {
        ESP_LOGW(TAG, "hci type %u len %u from host dropped", type, len);
    }
}

static esp_err_t bt_init(void) {
    if (esp_bt_controller_get_status() != ESP_BT_CONTROLLER_STATUS_IDLE) return ESP_OK;
    esp_bt_controller_config_t cfg = BT_CONTROLLER_INIT_CONFIG_DEFAULT();
    return esp_bt_controller_init(&cfg);
}

static esp_err_t bt_deinit(bool mem_release) {
    esp_err_t err = ESP_OK;
    if (esp_bt_controller_get_status() == ESP_BT_CONTROLLER_STATUS_INITED) {
        err = esp_bt_controller_deinit();
    }
    if (err == ESP_OK && mem_release) err = esp_bt_controller_mem_release(ESP_BT_MODE_BLE);
    return err;
}

/* The controller's own HCI glue copies every packet through malloc and asserts
 * when it runs out; replacing its callbacks keeps one copy into the SDIO buffer. */
static esp_err_t bt_enable(void) {
    if (esp_bt_controller_get_status() != ESP_BT_CONTROLLER_STATUS_ENABLED) {
        esp_err_t err = esp_bt_controller_enable(ESP_BT_MODE_BLE);
        if (err != ESP_OK) return err;
    }
    r_ble_hci_trans_cfg_hs(on_controller_evt, NULL, on_controller_acl, NULL);
    s_enabled = true;
    return ESP_OK;
}

static esp_err_t bt_disable(void) {
    if (esp_bt_controller_get_status() != ESP_BT_CONTROLLER_STATUS_ENABLED) return ESP_OK;
    s_enabled = false;
    return esp_bt_controller_disable();
}

void bt_rpc_feature_control(const Rpc *req, Rpc *resp) {
    const RpcReqFeatureControl *r = req->req_feature_control;
    RpcRespFeatureControl m = RPC__RESP__FEATURE_CONTROL__INIT;
    esp_err_t err = ESP_ERR_INVALID_ARG;
    if (r && r->feature == RPC_FEATURE__Feature_Bluetooth) {
        m.feature = r->feature;
        m.command = r->command;
        m.option = r->option;
        switch (r->command) {
        case RPC_FEATURE_COMMAND__Feature_Command_BT_Init:
            err = bt_init();
            break;
        case RPC_FEATURE_COMMAND__Feature_Command_BT_Deinit:
            err = bt_deinit(r->option == RPC_FEATURE_OPTION__Feature_Option_BT_Deinit_Release_Memory);
            break;
        case RPC_FEATURE_COMMAND__Feature_Command_BT_Enable:
            err = bt_enable();
            break;
        case RPC_FEATURE_COMMAND__Feature_Command_BT_Disable:
            err = bt_disable();
            break;
        default:
            break;
        }
    }
    if (err != ESP_OK) ESP_LOGE(TAG, "feature control %d: %s", r ? (int)r->command : -1, esp_err_to_name(err));
    m.resp = err;
    RPC_REPLY(resp, resp_feature_control, &m);
}
