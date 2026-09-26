/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "rpc.h"

#include <stdlib.h>

#include "esp_log.h"
#include "esp_system.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "ota.h"
#include "serial.h"
#include "sdkconfig.h"
#include "wifi.h"

#define QUEUE_DEPTH  8

static const char *TAG = "rpc";

typedef struct {
    uint8_t *msg;
    size_t len;
} rpc_msg_t;

typedef struct {
    RpcId id;
    rpc_handler_t fn;
    bool safe;
} rpc_entry_t;

static QueueHandle_t s_queue;
static bool s_safe_mode;

static void send_packed(bool event, Rpc *rpc) {
    size_t n = rpc__get_packed_size(rpc);
    uint8_t *buf = malloc(n);
    if (!buf) {
        ESP_LOGE(TAG, "no memory for rpc %d", rpc->msg_id);
        return;
    }
    rpc__pack(rpc, buf);
    serial_send(event, buf, n);
    free(buf);
}

void rpc_reply(Rpc *resp) {
    send_packed(false, resp);
}

void rpc_send_event(Rpc *event) {
    send_packed(true, event);
}

void rpc_send_esp_init(void) {
    RpcEventESPInit m = RPC__EVENT__ESPINIT__INIT;
    m.cp_reset_reason = esp_reset_reason();
    RPC_EVENT(RPC_ID__Event_ESPInit, event_esp_init, &m);
}

static void fw_version(const Rpc *req, Rpc *resp) {
    RpcRespGetCoprocessorFwVersion m = RPC__RESP__GET_COPROCESSOR_FW_VERSION__INIT;
    m.major1 = 2;
    m.minor1 = 12;
    m.patch1 = 6;
    m.chip_id = CONFIG_IDF_FIRMWARE_CHIP_ID;
    m.idf_target.data = (uint8_t *)CONFIG_IDF_TARGET;
    m.idf_target.len = sizeof(CONFIG_IDF_TARGET) - 1;
    RPC_REPLY(resp, resp_get_coprocessor_fwversion, &m);
}

static const rpc_entry_t s_handlers[] = {
    {RPC_ID__Req_GetCoprocessorFwVersion, fw_version, true},
    {RPC_ID__Req_OTABegin, ota_rpc_begin, true},
    {RPC_ID__Req_OTAWrite, ota_rpc_write, true},
    {RPC_ID__Req_OTAEnd, ota_rpc_end, true},
    {RPC_ID__Req_OTAActivate, ota_rpc_activate, true},
    {RPC_ID__Req_WifiInit, wifi_rpc_init, false},
    {RPC_ID__Req_WifiDeinit, wifi_rpc_deinit, false},
    {RPC_ID__Req_SetWifiMode, wifi_rpc_set_mode, false},
    {RPC_ID__Req_GetWifiMode, wifi_rpc_get_mode, false},
    {RPC_ID__Req_WifiSetStorage, wifi_rpc_set_storage, false},
    {RPC_ID__Req_GetMACAddress, wifi_rpc_get_mac, false},
    {RPC_ID__Req_SetMacAddress, wifi_rpc_set_mac, false},
    {RPC_ID__Req_WifiStart, wifi_rpc_start, false},
    {RPC_ID__Req_WifiStop, wifi_rpc_stop, false},
    {RPC_ID__Req_WifiSetConfig, wifi_rpc_set_config, false},
    {RPC_ID__Req_WifiGetConfig, wifi_rpc_get_config, false},
    {RPC_ID__Req_WifiConnect, wifi_rpc_connect, false},
    {RPC_ID__Req_WifiDisconnect, wifi_rpc_disconnect, false},
    {RPC_ID__Req_WifiSetPs, wifi_rpc_set_ps, false},
    {RPC_ID__Req_WifiGetPs, wifi_rpc_get_ps, false},
    {RPC_ID__Req_WifiScanStart, wifi_rpc_scan_start, false},
    {RPC_ID__Req_WifiScanStop, wifi_rpc_scan_stop, false},
    {RPC_ID__Req_WifiScanGetApNum, wifi_rpc_scan_get_ap_num, false},
    {RPC_ID__Req_WifiScanGetApRecords, wifi_rpc_scan_get_ap_records, false},
    {RPC_ID__Req_WifiStaGetApInfo, wifi_rpc_sta_get_ap_info, false},
    {RPC_ID__Req_WifiStaGetRssi, wifi_rpc_sta_get_rssi, false},
};

static void dispatch(const Rpc *req) {
    Rpc resp = RPC__INIT;
    resp.msg_type = RPC_TYPE__Resp;
    resp.msg_id = req->msg_id - RPC_ID__Req_Base + RPC_ID__Resp_Base;
    resp.uid = req->uid;

    for (size_t i = 0; i < sizeof(s_handlers) / sizeof(s_handlers[0]); i++) {
        if (s_handlers[i].id == req->msg_id && (s_handlers[i].safe || !s_safe_mode)) {
            s_handlers[i].fn(req, &resp);
            return;
        }
    }
    ESP_LOGW(TAG, "unsupported request %d", req->msg_id);
    resp.msg_id = RPC_ID__Resp_Base;
    rpc_reply(&resp);
}

static void rpc_task(void *arg) {
    for (;;) {
        rpc_msg_t m;
        xQueueReceive(s_queue, &m, portMAX_DELAY);
        Rpc *req = rpc__unpack(NULL, m.len, m.msg);
        free(m.msg);
        if (!req) {
            ESP_LOGW(TAG, "undecodable request dropped");
            continue;
        }
        /* The stock host leaves msg_type unset on requests. */
        if (req->msg_type != RPC_TYPE__Resp && req->msg_type != RPC_TYPE__Event) dispatch(req);
        rpc__free_unpacked(req, NULL);
    }
}

void rpc_on_message(uint8_t *msg, size_t len) {
    rpc_msg_t m = {msg, len};
    xQueueSend(s_queue, &m, portMAX_DELAY);
}

void rpc_init(bool safe_mode) {
    s_safe_mode = safe_mode;
    s_queue = xQueueCreate(QUEUE_DEPTH, sizeof(rpc_msg_t));
    xTaskCreate(rpc_task, "rpc", 6144, NULL, 20, NULL);
}
