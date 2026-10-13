/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * NimBLE host transport: HCI over HOSTED_IF_HCI.
 */

#include <string.h>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "hosted_host.h"
#include "host/ble_hs_mbuf.h"
#include "host/rpc.h"
#include "host/transport.h"
#include "nimble/hci_common.h"
#include "nimble/transport.h"
#include "nimble/transport_impl.h"
#include "os/os_mbuf.h"

#define TX_WAIT_MS   1000
#define ACL_WAIT_MS  100

static const char *TAG = "hosted_bt";

static volatile bool s_enabled;
static bool s_controlled;

static esp_err_t feature_control(RpcFeatureCommand command, RpcFeatureOption option) {
    RpcReqFeatureControl m = RPC__REQ__FEATURE_CONTROL__INIT;
    m.feature = RPC_FEATURE__Feature_Bluetooth;
    m.command = command;
    m.option = option;
    HOSTED_RPC_REQ(req, RPC_ID__Req_FeatureControl, req_feature_control, &m);
    return HOSTED_RPC_CALL_STATUS(req, resp_feature_control, HOSTED_RPC_TIMEOUT_MS);
}

/* Firmware 1.x enables its controller at boot and has no FeatureControl. */
static esp_err_t controller_up(void) {
    uint32_t major, minor, patch;
    esp_err_t err = hosted_host_fw_version(&major, &minor, &patch);
    if (err != ESP_OK) return err;
    s_controlled = major >= 2;
    if (!s_controlled) return ESP_OK;
    err = feature_control(RPC_FEATURE_COMMAND__Feature_Command_BT_Init, RPC_FEATURE_OPTION__Feature_Option_None);
    if (err == ESP_OK) {
        err = feature_control(RPC_FEATURE_COMMAND__Feature_Command_BT_Enable, RPC_FEATURE_OPTION__Feature_Option_None);
    }
    return err;
}

void ble_transport_ll_init(void) {
    hosted_rpc_init();
    esp_err_t err = hosted_transport_start();
    if (err == ESP_OK && !(hosted_transport_caps() & HOSTED_CAP_BT_SDIO)) err = ESP_ERR_NOT_SUPPORTED;
    if (err == ESP_OK) err = controller_up();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "coprocessor controller: %s", esp_err_to_name(err));
        return;
    }
    s_enabled = true;
}

void ble_transport_ll_deinit(void) {
    if (!s_enabled) return;
    s_enabled = false;
    if (!s_controlled) return;
    esp_err_t err = feature_control(RPC_FEATURE_COMMAND__Feature_Command_BT_Disable, RPC_FEATURE_OPTION__Feature_Option_None);
    if (err == ESP_OK) {
        err = feature_control(RPC_FEATURE_COMMAND__Feature_Command_BT_Deinit, RPC_FEATURE_OPTION__Feature_Option_None);
    }
    if (err != ESP_OK) ESP_LOGE(TAG, "coprocessor controller off: %s", esp_err_to_name(err));
}

static uint8_t *tx_get(void) {
    return s_enabled ? hosted_transport_tx_get(HOSTED_IF_HCI, pdMS_TO_TICKS(TX_WAIT_MS)) : NULL;
}

int ble_transport_to_ll_cmd_impl(void *buf) {
    size_t len = 3 + ((uint8_t *)buf)[2];
    uint8_t *p = tx_get();
    if (p) {
        memcpy(p, buf, len);
        hosted_transport_tx_put(p, HOSTED_IF_HCI, 0, HOSTED_HCI_CMD, len);
    } else {
        ESP_LOGE(TAG, "cmd 0x%04x not sent", ((uint8_t *)buf)[0] | ((uint8_t *)buf)[1] << 8);
    }
#if MYNEWT_VAL(MP_RUNTIME_ALLOC)
    ble_transport_free(BLE_HCI_CMD, buf);
#else
    ble_transport_free(buf);
#endif
    return p ? 0 : BLE_ERR_MEM_CAPACITY;
}

int ble_transport_to_ll_acl_impl(struct os_mbuf *om) {
    size_t len = OS_MBUF_PKTLEN(om);
    uint8_t *p = len <= HOSTED_MAX_PAYLOAD ? tx_get() : NULL;
    if (p) {
        os_mbuf_copydata(om, 0, len, p);
        hosted_transport_tx_put(p, HOSTED_IF_HCI, 0, HOSTED_HCI_ACL, len);
    } else {
        ESP_LOGE(TAG, "acl len %u not sent", (unsigned)len);
    }
    os_mbuf_free_chain(om);
    return p ? 0 : BLE_ERR_MEM_CAPACITY;
}

static void evt_to_host(const uint8_t *ev, size_t len) {
    if (len < 2 || len != 2u + ev[1] || len > MYNEWT_VAL(BLE_TRANSPORT_EVT_SIZE)) {
        ESP_LOGW(TAG, "malformed event, %u bytes", (unsigned)len);
        return;
    }
    bool adv = ev[0] == BLE_HCI_EVCODE_LE_META && len > 2 &&
               (ev[2] == BLE_HCI_LE_SUBEV_ADV_RPT || ev[2] == BLE_HCI_LE_SUBEV_EXT_ADV_RPT);
    uint8_t *buf = ble_transport_alloc_evt(adv);
    if (!buf) {
        if (!adv) ESP_LOGE(TAG, "no event buffer, event 0x%02x dropped", ev[0]);
        return;
    }
    memcpy(buf, ev, len);
    ble_transport_to_hs_evt(buf);
}

/* Waiting here stalls the whole link through the coprocessor's tokens; the
 * bound keeps a host task that waits on an HCI event from deadlocking it. */
static void acl_to_host(const uint8_t *data, size_t len) {
    TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(ACL_WAIT_MS);
    struct os_mbuf *om;
    while (!(om = ble_transport_alloc_acl_from_ll())) {
        if (!s_enabled || (int32_t)(deadline - xTaskGetTickCount()) <= 0) {
            ESP_LOGE(TAG, "no acl buffer, %u bytes dropped", (unsigned)len);
            return;
        }
        vTaskDelay(1);
    }
    if (os_mbuf_append(om, data, len) != 0) {
        ESP_LOGE(TAG, "acl of %u bytes does not fit", (unsigned)len);
        os_mbuf_free_chain(om);
        return;
    }
    ble_transport_to_hs_acl(om);
}

void hosted_bt_on_hci(const uint8_t *payload, uint16_t len) {
    if (!s_enabled || len < 2) return;
    if (payload[0] == HOSTED_HCI_EVT) {
        evt_to_host(payload + 1, len - 1);
    } else if (payload[0] == HOSTED_HCI_ACL) {
        acl_to_host(payload + 1, len - 1);
    }
}
