/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * RPC client. One request is in flight at a time; a response whose uid does
 * not match the pending request is dropped instead of being handed to the
 * next caller.
 */

#pragma once

#include <stdint.h>

#include "esp_err.h"
#include "esp_hosted_rpc.pb-c.h"

#define HOSTED_RPC_TIMEOUT_MS 5000

void hosted_rpc_init(void);

/* Fills msg_type and uid. Returns the decoded response (free with
 * rpc__free_unpacked) or NULL: ESP_ERR_TIMEOUT, ESP_ERR_NOT_SUPPORTED when the
 * coprocessor does not implement the request. */
Rpc *hosted_rpc_call(Rpc *req, uint32_t timeout_ms, esp_err_t *err);

/* Events, called from the transport rx task. */
void hosted_wifi_on_event(const Rpc *event);

#define HOSTED_RPC_REQ(name, id, field, payload) \
    Rpc name = RPC__INIT;                        \
    name.msg_id = (id);                          \
    name.payload_case = (Rpc__PayloadCase)(id);  \
    name.field = (payload)

#define HOSTED_RPC_REQ_NOARGS(name, id) \
    Rpc name = RPC__INIT;               \
    name.msg_id = (id)

#define HOSTED_RPC_STATUS(msg, field) ((msg)->field ? (msg)->field->resp : ESP_FAIL)

#define HOSTED_RPC_CALL_STATUS(req, field, timeout)          \
    ({                                                       \
        esp_err_t e_;                                        \
        Rpc *r_ = hosted_rpc_call(&(req), (timeout), &e_);   \
        if (r_) {                                            \
            e_ = HOSTED_RPC_STATUS(r_, field);               \
            rpc__free_unpacked(r_, NULL);                    \
        }                                                    \
        e_;                                                  \
    })
