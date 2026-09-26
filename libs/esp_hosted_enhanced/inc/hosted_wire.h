/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * esp-hosted 2.12 SDIO wire format, shared by the host and the coprocessor.
 */

#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct __attribute__((packed)) {
    uint8_t if_type : 4;
    uint8_t if_num : 4;
    uint8_t flags;
    uint16_t len;
    uint16_t offset;
    uint16_t checksum;
    uint16_t seq_num;
    uint8_t throttle_cmd : 2;
    uint8_t reserved : 6;
    uint8_t pkt_type;
} hosted_header_t;

#define HOSTED_BUF_SIZE     1536
#define HOSTED_MAX_PAYLOAD  (HOSTED_BUF_SIZE - sizeof(hosted_header_t))
#define HOSTED_MAX_SERIAL   8192

typedef enum {
    HOSTED_IF_INVALID = 0,
    HOSTED_IF_STA,
    HOSTED_IF_AP,
    HOSTED_IF_SERIAL,
    HOSTED_IF_HCI,
    HOSTED_IF_PRIV,
    HOSTED_IF_TEST,
} hosted_if_t;

#define HOSTED_FLAG_MORE_FRAGMENT  (1 << 0)
#define HOSTED_FLAG_WAKEUP_PKT     (1 << 1)

/* host -> slave: bit number written to the slave's CONF_W7 byte 0 */
typedef enum {
    HOSTED_H2S_OPEN_DATA_PATH = 0,
    HOSTED_H2S_CLOSE_DATA_PATH,
    HOSTED_H2S_RESET,
    HOSTED_H2S_POWER_SAVE_ON,
    HOSTED_H2S_POWER_SAVE_OFF,
} hosted_h2s_intr_t;

#define HOSTED_S2H_STOP_THROTTLE   6
#define HOSTED_S2H_START_THROTTLE  7

#define HOSTED_PRIV_PKT_EVENT      0x33
#define HOSTED_PRIV_EVENT_INIT     0x22

typedef enum {
    HOSTED_TLV_CAPABILITY = 0x11,
    HOSTED_TLV_CHIP_ID,
    HOSTED_TLV_TEST_RAW_TP,
    HOSTED_TLV_RX_Q_SIZE,
    HOSTED_TLV_TX_Q_SIZE,
    HOSTED_TLV_CAP_EXT,
    HOSTED_TLV_FW_VERSION,
    HOSTED_TLV_SDIO_MODE,
} hosted_init_tlv_t;

typedef enum {
    HOSTED_TLV_HOST_CAPABILITIES = 0x44,
    HOSTED_TLV_RCVD_CHIP_ID,
    HOSTED_TLV_SLV_TEST_RAW_TP,
    HOSTED_TLV_THROTTLE_HIGH,
    HOSTED_TLV_THROTTLE_LOW,
} hosted_config_tlv_t;

#define HOSTED_CAP_WLAN_SDIO  (1 << 0)

#define HOSTED_SERIAL_TLV_EP    0x01
#define HOSTED_SERIAL_TLV_DATA  0x02
#define HOSTED_SERIAL_EP_RSP    "RPCRsp"
#define HOSTED_SERIAL_EP_EVT    "RPCEvt"
#define HOSTED_SERIAL_EP_LEN    6

#ifdef __cplusplus
}
#endif
