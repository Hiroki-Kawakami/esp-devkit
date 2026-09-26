/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include "rpc.h"

void ota_rpc_begin(const Rpc *req, Rpc *resp);
void ota_rpc_write(const Rpc *req, Rpc *resp);
void ota_rpc_end(const Rpc *req, Rpc *resp);
void ota_rpc_activate(const Rpc *req, Rpc *resp);
