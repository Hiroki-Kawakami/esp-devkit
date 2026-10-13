/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "ble_bench.hpp"

#include "sdkconfig.h"

#if CONFIG_BT_NIMBLE_ENABLED

#include <algorithm>
#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "harness.h"
#include "host/ble_hs.h"
#include "host/ble_hs_hci.h"
#include "host/util/util.h"
#include "nimble/nimble_port.h"
#include "nimble/nimble_port_freertos.h"
#include "services/gap/ble_svc_gap.h"
#include "services/gatt/ble_svc_gatt.h"

#if !CONFIG_ESP_HOSTED_ENHANCED_HOST
#include "esp_hosted.h"
#include "esp_hosted_misc.h"
#endif

namespace {

constexpr char kName[] = "hosted-bench";
constexpr uint32_t kWaitMs = 10000;

const ble_uuid128_t kSvcUuid =
    BLE_UUID128_INIT(0x3a, 0x6b, 0x10, 0x52, 0x8e, 0x41, 0x4d, 0x0c, 0x9b, 0x5f, 0x21, 0x7e, 0x01, 0x00, 0xb0, 0x5e);
const ble_uuid128_t kChrUuid =
    BLE_UUID128_INIT(0x3a, 0x6b, 0x10, 0x52, 0x8e, 0x41, 0x4d, 0x0c, 0x9b, 0x5f, 0x21, 0x7e, 0x02, 0x00, 0xb0, 0x5e);

SemaphoreHandle_t s_event;
bool s_started;
uint8_t s_own_addr_type;
uint16_t s_chr_handle;
uint16_t s_peer_chr;
volatile uint16_t s_conn = BLE_HS_CONN_HANDLE_NONE;
volatile uint16_t s_mtu = BLE_ATT_MTU_DFLT;
volatile int s_status;
std::atomic<uint32_t> s_rx_bytes;
std::atomic<int64_t> s_rx_first;
std::atomic<int64_t> s_rx_last;
std::atomic<uint32_t> s_scan_reports;
uint8_t s_buf[BLE_ATT_ATTR_MAX_LEN];

void count_rx(uint32_t len) {
    int64_t now = esp_timer_get_time();
    if (s_rx_bytes.fetch_add(len) == 0) s_rx_first = now;
    s_rx_last = now;
}

int chr_access(uint16_t, uint16_t, ble_gatt_access_ctxt* ctxt, void*) {
    if (ctxt->op == BLE_GATT_ACCESS_OP_WRITE_CHR) count_rx(OS_MBUF_PKTLEN(ctxt->om));
    return 0;
}

ble_gatt_chr_def s_chrs[2];
ble_gatt_svc_def s_svcs[2];

void signal(int status) {
    s_status = status;
    xSemaphoreGive(s_event);
}

bool wait_event() {
    return xSemaphoreTake(s_event, pdMS_TO_TICKS(kWaitMs)) == pdTRUE && s_status == 0;
}

int gap_event(ble_gap_event* e, void*) {
    switch (e->type) {
    case BLE_GAP_EVENT_CONNECT:
        if (e->connect.status == 0) s_conn = e->connect.conn_handle;
        signal(e->connect.status);
        break;
    case BLE_GAP_EVENT_DISCONNECT:
        s_conn = BLE_HS_CONN_HANDLE_NONE;
        s_mtu = BLE_ATT_MTU_DFLT;
        break;
    case BLE_GAP_EVENT_MTU:
        s_mtu = e->mtu.value;
        break;
    case BLE_GAP_EVENT_NOTIFY_RX:
        count_rx(OS_MBUF_PKTLEN(e->notify_rx.om));
        break;
    case BLE_GAP_EVENT_DISC:
        s_scan_reports++;
        break;
    case BLE_GAP_EVENT_DISC_COMPLETE:
        signal(0);
        break;
    default:
        break;
    }
    return 0;
}

void on_sync() {
    signal(ble_hs_id_infer_auto(0, &s_own_addr_type));
}

void host_task(void*) {
    nimble_port_run();
    nimble_port_freertos_deinit();
}

bool reply_err(const char* what, int rc) {
    harness_reply("ERR ble %s: %d", what, rc);
    return true;
}

bool init() {
    int64_t t0 = esp_timer_get_time();
    if (!s_started) {
#if !CONFIG_ESP_HOSTED_ENHANCED_HOST
        esp_hosted_connect_to_slave();
        int err = esp_hosted_bt_controller_init();
        if (err == ESP_OK) err = esp_hosted_bt_controller_enable();
        if (err != ESP_OK) return reply_err("controller", err);
#endif
        int rc = nimble_port_init();
        if (rc != ESP_OK) return reply_err("nimble_port_init", rc);
        ble_svc_gap_init();
        ble_svc_gatt_init();
        ble_svc_gap_device_name_set(kName);
        s_chrs[0].uuid = &kChrUuid.u;
        s_chrs[0].access_cb = chr_access;
        s_chrs[0].flags = BLE_GATT_CHR_F_WRITE_NO_RSP | BLE_GATT_CHR_F_NOTIFY;
        s_chrs[0].val_handle = &s_chr_handle;
        s_svcs[0].type = BLE_GATT_SVC_TYPE_PRIMARY;
        s_svcs[0].uuid = &kSvcUuid.u;
        s_svcs[0].characteristics = s_chrs;
        rc = ble_gatts_count_cfg(s_svcs);
        if (rc == 0) rc = ble_gatts_add_svcs(s_svcs);
        if (rc != 0) return reply_err("gatts", rc);
        ble_hs_cfg.sync_cb = on_sync;
        xSemaphoreTake(s_event, 0);
        nimble_port_freertos_init(host_task);
        s_started = true;
        if (!wait_event()) return reply_err("sync", s_status);
    }
    uint8_t addr[6];
    int rc = ble_hs_id_copy_addr(s_own_addr_type, addr, nullptr);
    if (rc != 0) return reply_err("addr", rc);
    harness_reply("OK ble %u %02x:%02x:%02x:%02x:%02x:%02x %u", s_own_addr_type, addr[5], addr[4], addr[3],
                  addr[2], addr[1], addr[0], (unsigned)((esp_timer_get_time() - t0) / 1000));
    return true;
}

bool adv() {
    ble_hs_adv_fields fields = {};
    fields.flags = BLE_HS_ADV_F_DISC_GEN | BLE_HS_ADV_F_BREDR_UNSUP;
    fields.name = reinterpret_cast<const uint8_t*>(kName);
    fields.name_len = sizeof(kName) - 1;
    fields.name_is_complete = 1;
    int rc = ble_gap_adv_set_fields(&fields);
    if (rc != 0) return reply_err("adv fields", rc);
    ble_gap_adv_params params = {};
    params.conn_mode = BLE_GAP_CONN_MODE_UND;
    params.disc_mode = BLE_GAP_DISC_MODE_GEN;
    params.itvl_min = BLE_GAP_ADV_ITVL_MS(20);
    params.itvl_max = BLE_GAP_ADV_ITVL_MS(30);
    rc = ble_gap_adv_start(s_own_addr_type, nullptr, BLE_HS_FOREVER, &params, gap_event, nullptr);
    if (rc != 0 && rc != BLE_HS_EALREADY) return reply_err("adv", rc);
    harness_reply("OK ble adv");
    return true;
}

int mtu_cb(uint16_t, const ble_gatt_error* error, uint16_t mtu, void*) {
    if (error->status == 0) s_mtu = mtu;
    signal(error->status);
    return 0;
}

int disc_cb(uint16_t, const ble_gatt_error* error, const ble_gatt_chr* chr, void*) {
    if (error->status == 0) {
        s_peer_chr = chr->val_handle;
    } else {
        signal(error->status == BLE_HS_EDONE && s_peer_chr ? 0 : error->status);
    }
    return 0;
}

bool connect(int type, const char* str) {
    ble_addr_t peer = {};
    peer.type = type;
    unsigned b[6];
    if (sscanf(str, "%x:%x:%x:%x:%x:%x", &b[5], &b[4], &b[3], &b[2], &b[1], &b[0]) != 6) return false;
    for (int i = 0; i < 6; i++) peer.val[i] = b[i];
    ble_gap_conn_params params = {};
    params.scan_itvl = 0x10;
    params.scan_window = 0x10;
    params.itvl_min = BLE_GAP_CONN_ITVL_MS(7.5);
    params.itvl_max = BLE_GAP_CONN_ITVL_MS(15);
    params.supervision_timeout = BLE_GAP_SUPERVISION_TIMEOUT_MS(4000);
    int64_t t0 = esp_timer_get_time();
    xSemaphoreTake(s_event, 0);
    int rc = ble_gap_connect(s_own_addr_type, &peer, kWaitMs, &params, gap_event, nullptr);
    if (rc != 0) return reply_err("connect", rc);
    if (!wait_event()) return reply_err("connect event", s_status);
    uint32_t ms = (esp_timer_get_time() - t0) / 1000;

    ble_gap_set_prefered_le_phy(s_conn, BLE_GAP_LE_PHY_2M_MASK, BLE_GAP_LE_PHY_2M_MASK, 0);
    ble_gap_set_data_len(s_conn, 251, 2120);
    rc = ble_gattc_exchange_mtu(s_conn, mtu_cb, nullptr);
    if (rc != 0 || !wait_event()) return reply_err("mtu", rc ? rc : s_status);
    s_peer_chr = 0;
    rc = ble_gattc_disc_chrs_by_uuid(s_conn, 1, 0xffff, &kChrUuid.u, disc_cb, nullptr);
    if (rc != 0 || !wait_event()) return reply_err("discover", rc ? rc : s_status);
    harness_reply("OK ble connect %u %u", (unsigned)ms, (unsigned)s_mtu);
    return true;
}

template <typename Send>
bool stream(const char* what, uint32_t total, Send send) {
    if (s_conn == BLE_HS_CONN_HANDLE_NONE) return reply_err(what, BLE_HS_ENOTCONN);
    uint32_t chunk = std::min<uint32_t>(s_mtu - 3, sizeof(s_buf));
    uint32_t sent = 0;
    int64_t t0 = esp_timer_get_time();
    while (sent < total) {
        uint32_t n = std::min(chunk, total - sent);
        s_buf[0] = sent / chunk;
        int rc = send(n);
        if (rc == BLE_HS_ENOMEM || rc == BLE_HS_EAGAIN) {
            vTaskDelay(1);
            continue;
        }
        if (rc != 0) return reply_err(what, rc);
        sent += n;
    }
    harness_reply("OK ble %s %u %u", what, (unsigned)sent, (unsigned)(esp_timer_get_time() - t0));
    return true;
}

bool write(uint32_t total) {
    return stream("write", total, [](uint32_t n) {
        return ble_gattc_write_no_rsp_flat(s_conn, s_peer_chr, s_buf, n);
    });
}

bool notify(uint32_t total) {
    return stream("notify", total, [](uint32_t n) {
        os_mbuf* om = ble_hs_mbuf_from_flat(s_buf, n);
        if (!om) return static_cast<int>(BLE_HS_ENOMEM);
        return ble_gatts_notify_custom(s_conn, s_chr_handle, om);
    });
}

bool stats() {
    uint32_t bytes = s_rx_bytes.exchange(0);
    harness_reply("OK ble stats %u %u", (unsigned)bytes, bytes ? (unsigned)(s_rx_last - s_rx_first) : 0u);
    return true;
}

bool rtt(int count) {
    std::vector<uint32_t> us;
    us.reserve(count);
    for (int i = 0; i < count; i++) {
        uint8_t rnd[8];
        int64_t t0 = esp_timer_get_time();
        int rc = ble_hs_hci_util_rand(rnd, sizeof(rnd));
        if (rc != 0) return reply_err("rtt", rc);
        us.push_back(esp_timer_get_time() - t0);
    }
    std::sort(us.begin(), us.end());
    harness_reply("OK ble rtt %u %u %u", (unsigned)us[us.size() / 2], (unsigned)us[us.size() * 99 / 100],
                  (unsigned)us.back());
    return true;
}

bool scan(int ms) {
    ble_gap_disc_params params = {};
    params.passive = 1;
    params.filter_duplicates = 0;
    params.itvl = 0x30;
    params.window = 0x30;
    s_scan_reports = 0;
    xSemaphoreTake(s_event, 0);
    int rc = ble_gap_disc(s_own_addr_type, ms, &params, gap_event, nullptr);
    if (rc != 0) return reply_err("scan", rc);
    if (xSemaphoreTake(s_event, pdMS_TO_TICKS(ms + kWaitMs)) != pdTRUE) return reply_err("scan", BLE_HS_ETIMEOUT);
    harness_reply("OK ble scan %u", (unsigned)s_scan_reports.load());
    return true;
}

bool cmd_ble(int argc, const char* const* argv, void*) {
    if (argc < 2) return false;
    const char* sub = argv[1];
    if (!strcmp(sub, "init")) return init();
    if (!s_started) return reply_err(sub, BLE_HS_ENOTSYNCED);
    if (!strcmp(sub, "adv")) return adv();
    if (!strcmp(sub, "connect") && argc == 4) return connect(atoi(argv[2]), argv[3]);
    if (!strcmp(sub, "write") && argc == 3) return write(strtoul(argv[2], nullptr, 0));
    if (!strcmp(sub, "notify") && argc == 3) return notify(strtoul(argv[2], nullptr, 0));
    if (!strcmp(sub, "stats")) return stats();
    if (!strcmp(sub, "rtt") && argc == 3) return rtt(std::max(1, atoi(argv[2])));
    if (!strcmp(sub, "scan") && argc == 3) return scan(atoi(argv[2]));
    if (!strcmp(sub, "disconnect")) {
        if (s_conn != BLE_HS_CONN_HANDLE_NONE) ble_gap_terminate(s_conn, BLE_ERR_REM_USER_CONN_TERM);
        harness_reply("OK ble disconnect");
        return true;
    }
    return false;
}

}  // namespace

void ble_bench_start() {
    s_event = xSemaphoreCreateBinary();
    harness_register("ble", cmd_ble, nullptr);
}

#else

void ble_bench_start() {}

#endif
