/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * GT911 HotKnot — internal to the gt911 driver dir: chip primitives shared by
 * gt911_hotknot.c + gt911_hotknot_fw_loader.c, plus gt911_hotknot_create() (the
 * board registers it as the BSP provider). App-facing API is bsp_hotknot_* (bsp.h).
 */

#pragma once
#include "gt911_internal.h"
#include "bsp_hotknot.h"   /* inc_private vtable */

#ifdef __cplusplus
extern "C" {
#endif

#define GT911_HOTKNOT_MAX_PAYLOAD  128

/* Approach command written to Command (0x8040). The two ends must be opposite
 * roles to pair, which is why the BSP keeps a role on begin(). */
typedef enum {
    GT911_HOTKNOT_ROLE_SLAVE  = 0x20,
    GT911_HOTKNOT_ROLE_MASTER = 0x21,
} gt911_hotknot_role_t;

/* GT911_Status (0x81A8) values. */
typedef enum {
    GT911_HOTKNOT_MODE_IDLE    = 0x00,
    GT911_HOTKNOT_MODE_SLAVE   = 0x88,
    GT911_HOTKNOT_MODE_MASTER  = 0x99,
    GT911_HOTKNOT_MODE_RECEIVE = 0xAA,
    GT911_HOTKNOT_MODE_SEND    = 0xBB,
    GT911_HOTKNOT_MODE_UNKNOWN = 0xFF,
} gt911_hotknot_mode_t;

/* Send/receive completion codes (programming guide §3.6). 0x01/0x02/0x05 are
 * intermediate and never surface here. */
typedef enum {
    GT911_HOTKNOT_XFER_OK            = 0x03,
    GT911_HOTKNOT_XFER_VERIFY_ERROR  = 0x04,
    GT911_HOTKNOT_XFER_PEER_DEPARTED = 0x07,
} gt911_hotknot_xfer_status_t;

/* ----- Chip-level primitives --------------------------------------------- */

/* Force HotKnot_En + Approch_En and apply the board tuning. RAM, not NVM — re-run
 * before each session. */
esp_err_t gt911_hotknot_enable_in_config(gt911_handle_t handle);

/* Enter approach (slave 0x20 / master 0x21); touch keeps working. */
esp_err_t gt911_hotknot_enter_approach(gt911_handle_t handle, gt911_hotknot_role_t role);

/* Leave the active mode (UNKNOWN fires all exit commands). */
esp_err_t gt911_hotknot_exit(gt911_handle_t handle, gt911_hotknot_mode_t current_mode);

/* Read GT911_Status (0x81A8) with backup-byte match. */
esp_err_t gt911_hotknot_get_mode(gt911_handle_t handle, gt911_hotknot_mode_t *out_mode);

/* Stream the 16 KB transmission firmware into chip SRAM; chip then enters RECEIVE
 * and touch is dead until recover. */
esp_err_t gt911_hotknot_load_subsystem(gt911_handle_t handle);

/* Send 1..128 even bytes; blocks up to timeout_ms, *out_status is the chip verdict,
 * event flag acked internally. */
esp_err_t gt911_hotknot_send(gt911_handle_t handle, const uint8_t *data, size_t len,
                             uint32_t timeout_ms, uint8_t *out_status);

/* Non-blocking: if a frame is buffered, copy it CRC-checked, ack, *got=true.
 * INVALID_CRC / INVALID_SIZE on a bad frame. */
esp_err_t gt911_hotknot_poll_receive(gt911_handle_t handle, uint8_t *buffer, size_t buffer_size,
                                     size_t *out_len, bool *got);

/* Hard-reset + int-sync back to touch mode (needs board RESET/INT wiring). */
esp_err_t gt911_hotknot_recover_touch(gt911_handle_t handle);

/* ----- BSP provider ------------------------------------------------------ */

/* Provider bound to the active GT911; ESP_ERR_INVALID_STATE if none has a running dispatch task. */
esp_err_t gt911_hotknot_create(bsp_hotknot_t **out_hk);

#ifdef __cplusplus
}
#endif
