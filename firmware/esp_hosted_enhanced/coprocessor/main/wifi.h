/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <stdint.h>

#include "rpc.h"

/* Host -> air, from the transport rx task. */
void wifi_on_host_packet(uint8_t *frame, uint16_t len);

void wifi_rpc_init(const Rpc *req, Rpc *resp);
void wifi_rpc_deinit(const Rpc *req, Rpc *resp);
void wifi_rpc_set_mode(const Rpc *req, Rpc *resp);
void wifi_rpc_get_mode(const Rpc *req, Rpc *resp);
void wifi_rpc_set_storage(const Rpc *req, Rpc *resp);
void wifi_rpc_get_mac(const Rpc *req, Rpc *resp);
void wifi_rpc_set_mac(const Rpc *req, Rpc *resp);
void wifi_rpc_start(const Rpc *req, Rpc *resp);
void wifi_rpc_stop(const Rpc *req, Rpc *resp);
void wifi_rpc_set_config(const Rpc *req, Rpc *resp);
void wifi_rpc_get_config(const Rpc *req, Rpc *resp);
void wifi_rpc_connect(const Rpc *req, Rpc *resp);
void wifi_rpc_disconnect(const Rpc *req, Rpc *resp);
void wifi_rpc_set_ps(const Rpc *req, Rpc *resp);
void wifi_rpc_get_ps(const Rpc *req, Rpc *resp);
void wifi_rpc_scan_start(const Rpc *req, Rpc *resp);
void wifi_rpc_scan_stop(const Rpc *req, Rpc *resp);
void wifi_rpc_scan_get_ap_num(const Rpc *req, Rpc *resp);
void wifi_rpc_scan_get_ap_records(const Rpc *req, Rpc *resp);
void wifi_rpc_sta_get_ap_info(const Rpc *req, Rpc *resp);
void wifi_rpc_sta_get_rssi(const Rpc *req, Rpc *resp);
