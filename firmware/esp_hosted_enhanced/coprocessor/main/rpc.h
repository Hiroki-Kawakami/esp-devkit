/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * RPC dispatch. Requests run one at a time on the rpc task, and every request
 * gets exactly one response: the host matches responses through a shared FIFO,
 * so a missing or late one is handed to the next caller.
 */

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_hosted_rpc.pb-c.h"

/* The handler must call RPC_REPLY exactly once. */
typedef void (*rpc_handler_t)(const Rpc *req, Rpc *resp);

/* Safe mode serves only the version and OTA requests. */
void rpc_init(bool safe_mode);
void rpc_on_message(uint8_t *msg, size_t len);
void rpc_send_esp_init(void);

void rpc_reply(Rpc *resp);
void rpc_send_event(Rpc *event);

#define RPC_REPLY(resp, field, msg)                                  \
    do {                                                             \
        (resp)->payload_case = (Rpc__PayloadCase)(resp)->msg_id;     \
        (resp)->field = (msg);                                       \
        rpc_reply(resp);                                             \
    } while (0)

#define RPC_EVENT(id, field, msg)                        \
    do {                                                 \
        Rpc e_ = RPC__INIT;                              \
        e_.msg_type = RPC_TYPE__Event;                   \
        e_.msg_id = (id);                                \
        e_.payload_case = (Rpc__PayloadCase)(id);        \
        e_.field = (msg);                                \
        rpc_send_event(&e_);                             \
    } while (0)

#define RPC_REPLY_STATUS(resp, type, init, field, err) \
    do {                                               \
        type m_ = init;                                \
        m_.resp = (err);                               \
        RPC_REPLY(resp, field, &m_);                   \
    } while (0)
