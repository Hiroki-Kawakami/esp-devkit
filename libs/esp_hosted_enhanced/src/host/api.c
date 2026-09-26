/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "hosted_host.h"

#include "host/rpc.h"
#include "host/transport.h"

#define OTA_TIMEOUT_MS 30000

esp_err_t hosted_host_connect(void) {
    hosted_rpc_init();
    return hosted_transport_start();
}

esp_err_t hosted_host_fw_version(uint32_t *major, uint32_t *minor, uint32_t *patch) {
    RpcReqGetCoprocessorFwVersion m = RPC__REQ__GET_COPROCESSOR_FW_VERSION__INIT;
    HOSTED_RPC_REQ(req, RPC_ID__Req_GetCoprocessorFwVersion, req_get_coprocessor_fwversion, &m);
    esp_err_t err;
    Rpc *resp = hosted_rpc_call(&req, HOSTED_RPC_TIMEOUT_MS, &err);
    if (!resp) return err;
    const RpcRespGetCoprocessorFwVersion *r = resp->resp_get_coprocessor_fwversion;
    err = r ? r->resp : ESP_FAIL;
    if (err == ESP_OK) {
        *major = r->major1;
        *minor = r->minor1;
        *patch = r->patch1;
    }
    rpc__free_unpacked(resp, NULL);
    return err;
}


esp_err_t hosted_host_ota_begin(void) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_OTABegin);
    return HOSTED_RPC_CALL_STATUS(req, resp_ota_begin, OTA_TIMEOUT_MS);
}

esp_err_t hosted_host_ota_write(const void *data, size_t len) {
    RpcReqOTAWrite m = RPC__REQ__OTAWRITE__INIT;
    m.ota_data = (ProtobufCBinaryData){len, (uint8_t *)data};
    HOSTED_RPC_REQ(req, RPC_ID__Req_OTAWrite, req_ota_write, &m);
    return HOSTED_RPC_CALL_STATUS(req, resp_ota_write, OTA_TIMEOUT_MS);
}

esp_err_t hosted_host_ota_end(void) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_OTAEnd);
    return HOSTED_RPC_CALL_STATUS(req, resp_ota_end, OTA_TIMEOUT_MS);
}

esp_err_t hosted_host_ota_activate(void) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_OTAActivate);
    return HOSTED_RPC_CALL_STATUS(req, resp_ota_activate, HOSTED_RPC_TIMEOUT_MS);
}
