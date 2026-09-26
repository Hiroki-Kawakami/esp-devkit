/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "ota.h"

#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_system.h"
#include "freertos/FreeRTOS.h"
#include "freertos/timers.h"

#define RESTART_DELAY_MS 1000

static const char *TAG = "ota";

static esp_ota_handle_t s_handle;
static const esp_partition_t *s_slot;
static bool s_complete;

void ota_rpc_begin(const Rpc *req, Rpc *resp) {
    if (s_handle) {
        esp_ota_abort(s_handle);
        s_handle = 0;
    }
    s_complete = false;
    s_slot = esp_ota_get_next_update_partition(NULL);
    esp_err_t err = ESP_ERR_NOT_FOUND;
    if (s_slot) {
        err = esp_ota_begin(s_slot, OTA_WITH_SEQUENTIAL_WRITES, &s_handle);
        if (err != ESP_OK) s_handle = 0;
    }
    ESP_LOGI(TAG, "begin %s: %s", s_slot ? s_slot->label : "-", esp_err_to_name(err));
    RPC_REPLY_STATUS(resp, RpcRespOTABegin, RPC__RESP__OTABEGIN__INIT, resp_ota_begin, err);
}

void ota_rpc_write(const Rpc *req, Rpc *resp) {
    const RpcReqOTAWrite *r = req->req_ota_write;
    esp_err_t err = ESP_ERR_INVALID_STATE;
    if (!r) {
        err = ESP_ERR_INVALID_ARG;
    } else if (s_handle) {
        err = esp_ota_write(s_handle, r->ota_data.data, r->ota_data.len);
    }
    RPC_REPLY_STATUS(resp, RpcRespOTAWrite, RPC__RESP__OTAWRITE__INIT, resp_ota_write, err);
}

void ota_rpc_end(const Rpc *req, Rpc *resp) {
    esp_err_t err = ESP_ERR_INVALID_STATE;
    if (s_handle) {
        err = esp_ota_end(s_handle);
        s_handle = 0;
        s_complete = err == ESP_OK;
    }
    ESP_LOGI(TAG, "end: %s", esp_err_to_name(err));
    RPC_REPLY_STATUS(resp, RpcRespOTAEnd, RPC__RESP__OTAEND__INIT, resp_ota_end, err);
}

static void restart_cb(TimerHandle_t t) {
    esp_restart();
}

void ota_rpc_activate(const Rpc *req, Rpc *resp) {
    esp_err_t err = s_complete ? esp_ota_set_boot_partition(s_slot) : ESP_ERR_INVALID_STATE;
    RPC_REPLY_STATUS(resp, RpcRespOTAActivate, RPC__RESP__OTAACTIVATE__INIT, resp_ota_activate, err);
    if (err == ESP_OK) {
        ESP_LOGI(TAG, "activated %s, restarting", s_slot->label);
        xTimerStart(xTimerCreate("ota_rst", pdMS_TO_TICKS(RESTART_DELAY_MS), pdFALSE, NULL, restart_cb), 0);
    }
}
