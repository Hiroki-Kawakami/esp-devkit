/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * esp_wifi_remote over the coprocessor RPCs, plus the esp_wifi internal data
 * hooks that the IDF remote glue declares weak.
 */

#include <stdlib.h>
#include <string.h>

#include "esp_event.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_private/wifi.h"
#include "esp_wifi.h"
#include "esp_wifi_remote.h"
#include "host/rpc.h"
#include "host/transport.h"

#define INIT_TIMEOUT_MS       10000
#define SCAN_BLOCK_TIMEOUT_MS 30000
#define EVENT_POST_WAIT       pdMS_TO_TICKS(100)
#define TX_WAIT               pdMS_TO_TICKS(20)

enum {
    AP_REC_11B = 0,
    AP_REC_11G,
    AP_REC_11N,
    AP_REC_LR,
    AP_REC_11AX,
    AP_REC_WPS,
    AP_REC_FTM_RESPONDER,
    AP_REC_FTM_INITIATOR,
    AP_REC_11A,
    AP_REC_11AC,
};

enum {
    STA_CFG_RM = 0,
    STA_CFG_BTM,
    STA_CFG_MBO,
    STA_CFG_FT,
    STA_CFG_OWE,
    STA_CFG_TRANSITION_DISABLE,
};

enum {
    STA_HE_DCM_SET = 0,
    STA_HE_DCM_MAX_TX = 1,
    STA_HE_DCM_MAX_RX = 3,
    STA_HE_MCS9 = 5,
    STA_HE_SU_BMFEE_DISABLED,
    STA_HE_TRIG_SU_FB_DISABLED,
    STA_HE_TRIG_MU_FB_DISABLED,
    STA_HE_TRIG_CQI_FB_DISABLED,
    STA_VHT_SU_BMFEE_DISABLED,
    STA_VHT_MU_BMFEE_DISABLED,
    STA_VHT_MCS8,
};

#define BIT_OF(v, pos) (((v) >> (pos)) & 1)

static const char *TAG = "hosted_wifi";

static wifi_rxcb_t s_sta_rx;
static bool s_sta_started;
static bool s_sta_connected;

esp_err_t esp_wifi_internal_reg_rxcb(wifi_interface_t ifx, wifi_rxcb_t fn) {
    if (ifx != WIFI_IF_STA) return ESP_ERR_NOT_SUPPORTED;
    s_sta_rx = fn;
    return ESP_OK;
}

void hosted_wifi_on_sta(const uint8_t *frame, uint16_t len) {
    wifi_rxcb_t rx = s_sta_rx;
    if (!rx) return;
    void *buf = heap_caps_malloc(len, MALLOC_CAP_SPIRAM);
    if (!buf) return;
    memcpy(buf, frame, len);
    rx(buf, len, buf);
}

void esp_wifi_internal_free_rx_buffer(void *buffer) {
    free(buffer);
}

int esp_wifi_internal_tx(wifi_interface_t ifx, void *buffer, uint16_t len) {
    if (ifx != WIFI_IF_STA) return ESP_ERR_NOT_SUPPORTED;
    return hosted_transport_send(HOSTED_IF_STA, 0, buffer, len, TX_WAIT);
}

esp_err_t esp_wifi_internal_tx_by_ref(wifi_interface_t ifx, void *buffer, size_t len, void *netstack_buf) {
    return esp_wifi_internal_tx(ifx, buffer, len);
}

esp_err_t esp_wifi_internal_reg_netstack_buf_cb(wifi_netstack_buf_ref_cb_t ref, wifi_netstack_buf_free_cb_t free_cb) {
    return ESP_OK;
}

esp_err_t esp_wifi_internal_set_sta_ip(void) {
    return ESP_OK;
}

static void post(int32_t id, const void *data, size_t size) {
    if (esp_event_post(WIFI_EVENT, id, data, size, EVENT_POST_WAIT) != ESP_OK) {
        ESP_LOGW(TAG, "event %d dropped", (int)id);
    }
}

static void copy_bytes(void *dst, size_t cap, const ProtobufCBinaryData *src) {
    memcpy(dst, src->data, src->len < cap ? src->len : cap);
}

void hosted_wifi_on_event(const Rpc *ev) {
    switch (ev->msg_id) {
    case RPC_ID__Event_WifiEventNoArgs: {
        const RpcEventWifiEventNoArgs *e = ev->event_wifi_event_no_args;
        if (!e) break;
        if (e->event_id == WIFI_EVENT_STA_START) {
            if (s_sta_started) break;
            s_sta_started = true;
        } else if (e->event_id == WIFI_EVENT_STA_STOP) {
            s_sta_started = false;
            s_sta_connected = false;
        } else if (e->event_id != WIFI_EVENT_HOME_CHANNEL_CHANGE) {
            break;
        }
        post(e->event_id, NULL, 0);
        break;
    }
    case RPC_ID__Event_StaConnected: {
        const WifiEventStaConnected *c = ev->event_sta_connected ? ev->event_sta_connected->sta_connected : NULL;
        if (!c || !s_sta_started || s_sta_connected) break;
        wifi_event_sta_connected_t e = {
            .ssid_len = c->ssid_len,
            .channel = c->channel,
            .authmode = c->authmode,
            .aid = c->aid,
        };
        copy_bytes(e.ssid, sizeof(e.ssid), &c->ssid);
        copy_bytes(e.bssid, sizeof(e.bssid), &c->bssid);
        s_sta_connected = true;
        post(WIFI_EVENT_STA_CONNECTED, &e, sizeof(e));
        break;
    }
    case RPC_ID__Event_StaDisconnected: {
        const WifiEventStaDisconnected *d = ev->event_sta_disconnected ? ev->event_sta_disconnected->sta_disconnected : NULL;
        if (!d) break;
        wifi_event_sta_disconnected_t e = {
            .ssid_len = d->ssid_len,
            .reason = d->reason,
            .rssi = d->rssi,
        };
        copy_bytes(e.ssid, sizeof(e.ssid), &d->ssid);
        copy_bytes(e.bssid, sizeof(e.bssid), &d->bssid);
        s_sta_connected = false;
        post(WIFI_EVENT_STA_DISCONNECTED, &e, sizeof(e));
        break;
    }
    case RPC_ID__Event_StaScanDone: {
        const WifiEventStaScanDone *s = ev->event_sta_scan_done ? ev->event_sta_scan_done->scan_done : NULL;
        if (!s) break;
        wifi_event_sta_scan_done_t e = {
            .status = s->status,
            .number = s->number,
            .scan_id = s->scan_id,
        };
        post(WIFI_EVENT_SCAN_DONE, &e, sizeof(e));
        break;
    }
    case RPC_ID__Event_ESPInit:
        ESP_LOGI(TAG, "coprocessor up");
        break;
    default:
        break;
    }
}

esp_err_t esp_wifi_remote_init(const wifi_init_config_t *c) {
    hosted_rpc_init();
    esp_err_t err = hosted_transport_start();
    if (err != ESP_OK) return err;

    WifiInitConfig cfg = WIFI_INIT_CONFIG__INIT;
    cfg.static_rx_buf_num = c->static_rx_buf_num;
    cfg.dynamic_rx_buf_num = c->dynamic_rx_buf_num;
    cfg.tx_buf_type = c->tx_buf_type;
    cfg.static_tx_buf_num = c->static_tx_buf_num;
    cfg.dynamic_tx_buf_num = c->dynamic_tx_buf_num;
    cfg.cache_tx_buf_num = c->cache_tx_buf_num;
    cfg.csi_enable = c->csi_enable;
    cfg.ampdu_rx_enable = c->ampdu_rx_enable;
    cfg.ampdu_tx_enable = c->ampdu_tx_enable;
    cfg.amsdu_tx_enable = c->amsdu_tx_enable;
    cfg.nvs_enable = c->nvs_enable;
    cfg.nano_enable = c->nano_enable;
    cfg.rx_ba_win = c->rx_ba_win;
    cfg.wifi_task_core_id = c->wifi_task_core_id;
    cfg.beacon_max_len = c->beacon_max_len;
    cfg.mgmt_sbuf_num = c->mgmt_sbuf_num;
    cfg.feature_caps = c->feature_caps;
    cfg.sta_disconnected_pm = c->sta_disconnected_pm;
    cfg.espnow_max_encrypt_num = c->espnow_max_encrypt_num;
    cfg.magic = c->magic;
    cfg.rx_mgmt_buf_type = c->rx_mgmt_buf_type;
    cfg.rx_mgmt_buf_num = c->rx_mgmt_buf_num;
    cfg.tx_hetb_queue_num = c->tx_hetb_queue_num;
    cfg.dump_hesigb_enable = c->dump_hesigb_enable;
    RpcReqWifiInit m = RPC__REQ__WIFI_INIT__INIT;
    m.cfg = &cfg;
    HOSTED_RPC_REQ(req, RPC_ID__Req_WifiInit, req_wifi_init, &m);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_init, INIT_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_deinit(void) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiDeinit);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_deinit, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_set_mode(wifi_mode_t mode) {
    RpcReqSetMode m = RPC__REQ__SET_MODE__INIT;
    m.mode = mode;
    HOSTED_RPC_REQ(req, RPC_ID__Req_SetWifiMode, req_set_wifi_mode, &m);
    return HOSTED_RPC_CALL_STATUS(req, resp_set_wifi_mode, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_get_mode(wifi_mode_t *mode) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_GetWifiMode);
    esp_err_t err;
    Rpc *resp = hosted_rpc_call(&req, HOSTED_RPC_TIMEOUT_MS, &err);
    if (!resp) return err;
    err = HOSTED_RPC_STATUS(resp, resp_get_wifi_mode);
    if (err == ESP_OK) *mode = resp->resp_get_wifi_mode->mode;
    rpc__free_unpacked(resp, NULL);
    return err;
}

esp_err_t esp_wifi_remote_set_storage(wifi_storage_t storage) {
    RpcReqWifiSetStorage m = RPC__REQ__WIFI_SET_STORAGE__INIT;
    m.storage = storage;
    HOSTED_RPC_REQ(req, RPC_ID__Req_WifiSetStorage, req_wifi_set_storage, &m);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_set_storage, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_get_mac(wifi_interface_t ifx, uint8_t mac[6]) {
    RpcReqGetMacAddress m = RPC__REQ__GET_MAC_ADDRESS__INIT;
    m.mode = ifx;
    HOSTED_RPC_REQ(req, RPC_ID__Req_GetMACAddress, req_get_mac_address, &m);
    esp_err_t err;
    Rpc *resp = hosted_rpc_call(&req, HOSTED_RPC_TIMEOUT_MS, &err);
    if (!resp) return err;
    err = HOSTED_RPC_STATUS(resp, resp_get_mac_address);
    if (err == ESP_OK) {
        if (resp->resp_get_mac_address->mac.len == 6) {
            memcpy(mac, resp->resp_get_mac_address->mac.data, 6);
        } else {
            err = ESP_FAIL;
        }
    }
    rpc__free_unpacked(resp, NULL);
    return err;
}

esp_err_t esp_wifi_remote_set_mac(wifi_interface_t ifx, const uint8_t mac[6]) {
    RpcReqSetMacAddress m = RPC__REQ__SET_MAC_ADDRESS__INIT;
    m.mode = ifx;
    m.mac = (ProtobufCBinaryData){6, (uint8_t *)mac};
    HOSTED_RPC_REQ(req, RPC_ID__Req_SetMacAddress, req_set_mac_address, &m);
    return HOSTED_RPC_CALL_STATUS(req, resp_set_mac_address, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_start(void) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiStart);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_start, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_stop(void) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiStop);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_stop, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_connect(void) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiConnect);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_connect, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_disconnect(void) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiDisconnect);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_disconnect, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_set_ps(wifi_ps_type_t type) {
    RpcReqSetPs m = RPC__REQ__SET_PS__INIT;
    m.type = type;
    HOSTED_RPC_REQ(req, RPC_ID__Req_WifiSetPs, req_wifi_set_ps, &m);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_set_ps, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_get_ps(wifi_ps_type_t *type) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiGetPs);
    esp_err_t err;
    Rpc *resp = hosted_rpc_call(&req, HOSTED_RPC_TIMEOUT_MS, &err);
    if (!resp) return err;
    err = HOSTED_RPC_STATUS(resp, resp_wifi_get_ps);
    if (err == ESP_OK) *type = resp->resp_wifi_get_ps->type;
    rpc__free_unpacked(resp, NULL);
    return err;
}

static size_t cstr_len(const uint8_t *s, size_t cap) {
    return strnlen((const char *)s, cap);
}

esp_err_t esp_wifi_remote_set_config(wifi_interface_t ifx, wifi_config_t *conf) {
    if (ifx != WIFI_IF_STA) return ESP_ERR_NOT_SUPPORTED;
    const wifi_sta_config_t *a = &conf->sta;
    WifiScanThreshold th = WIFI_SCAN_THRESHOLD__INIT;
    th.rssi = a->threshold.rssi;
    th.authmode = a->threshold.authmode;
    th.rssi_5g_adjustment = a->threshold.rssi_5g_adjustment;
    WifiPmfConfig pmf = WIFI_PMF_CONFIG__INIT;
    pmf.capable = a->pmf_cfg.capable;
    pmf.required = a->pmf_cfg.required;
    WifiStaConfig sc = WIFI_STA_CONFIG__INIT;
    sc.ssid = (ProtobufCBinaryData){cstr_len(a->ssid, sizeof(a->ssid)), (uint8_t *)a->ssid};
    sc.password = (ProtobufCBinaryData){cstr_len(a->password, sizeof(a->password)), (uint8_t *)a->password};
    sc.scan_method = a->scan_method;
    sc.bssid_set = a->bssid_set;
    sc.bssid = (ProtobufCBinaryData){sizeof(a->bssid), (uint8_t *)a->bssid};
    sc.channel = a->channel;
    sc.listen_interval = a->listen_interval;
    sc.sort_method = a->sort_method;
    sc.threshold = &th;
    sc.pmf_cfg = &pmf;
    sc.bitmask = a->rm_enabled << STA_CFG_RM | a->btm_enabled << STA_CFG_BTM |
                 a->mbo_enabled << STA_CFG_MBO | a->ft_enabled << STA_CFG_FT |
                 a->owe_enabled << STA_CFG_OWE |
                 a->transition_disable << STA_CFG_TRANSITION_DISABLE;
    sc.sae_pwe_h2e = a->sae_pwe_h2e;
    sc.failure_retry_cnt = a->failure_retry_cnt;
    sc.he_bitmask = a->he_dcm_set << STA_HE_DCM_SET |
                    a->he_dcm_max_constellation_tx << STA_HE_DCM_MAX_TX |
                    a->he_dcm_max_constellation_rx << STA_HE_DCM_MAX_RX |
                    a->he_mcs9_enabled << STA_HE_MCS9 |
                    a->he_su_beamformee_disabled << STA_HE_SU_BMFEE_DISABLED |
                    a->he_trig_su_bmforming_feedback_disabled << STA_HE_TRIG_SU_FB_DISABLED |
                    a->he_trig_mu_bmforming_partial_feedback_disabled << STA_HE_TRIG_MU_FB_DISABLED |
                    a->he_trig_cqi_feedback_disabled << STA_HE_TRIG_CQI_FB_DISABLED |
                    a->vht_su_beamformee_disabled << STA_VHT_SU_BMFEE_DISABLED |
                    a->vht_mu_beamformee_disabled << STA_VHT_MU_BMFEE_DISABLED |
                    a->vht_mcs8_enabled << STA_VHT_MCS8;
    sc.sae_h2e_identifier = (ProtobufCBinaryData){
        cstr_len(a->sae_h2e_identifier, sizeof(a->sae_h2e_identifier)), (uint8_t *)a->sae_h2e_identifier};
    sc.sae_pk_mode = a->sae_pk_mode;
    WifiConfig wc = WIFI_CONFIG__INIT;
    wc.u_case = WIFI_CONFIG__U_STA;
    wc.sta = &sc;
    RpcReqWifiSetConfig m = RPC__REQ__WIFI_SET_CONFIG__INIT;
    m.iface = ifx;
    m.cfg = &wc;
    HOSTED_RPC_REQ(req, RPC_ID__Req_WifiSetConfig, req_wifi_set_config, &m);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_set_config, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_get_config(wifi_interface_t ifx, wifi_config_t *conf) {
    if (ifx != WIFI_IF_STA) return ESP_ERR_NOT_SUPPORTED;
    RpcReqWifiGetConfig m = RPC__REQ__WIFI_GET_CONFIG__INIT;
    m.iface = ifx;
    HOSTED_RPC_REQ(req, RPC_ID__Req_WifiGetConfig, req_wifi_get_config, &m);
    esp_err_t err;
    Rpc *resp = hosted_rpc_call(&req, HOSTED_RPC_TIMEOUT_MS, &err);
    if (!resp) return err;
    err = HOSTED_RPC_STATUS(resp, resp_wifi_get_config);
    const WifiConfig *wc = err == ESP_OK ? resp->resp_wifi_get_config->cfg : NULL;
    if (err == ESP_OK && (!wc || wc->u_case != WIFI_CONFIG__U_STA)) err = ESP_FAIL;
    if (err == ESP_OK) {
        const WifiStaConfig *c = wc->sta;
        wifi_sta_config_t *a = &conf->sta;
        memset(conf, 0, sizeof(*conf));
        copy_bytes(a->ssid, sizeof(a->ssid), &c->ssid);
        copy_bytes(a->password, sizeof(a->password), &c->password);
        a->scan_method = c->scan_method;
        a->bssid_set = c->bssid_set;
        copy_bytes(a->bssid, sizeof(a->bssid), &c->bssid);
        a->channel = c->channel;
        a->listen_interval = c->listen_interval;
        a->sort_method = c->sort_method;
        if (c->threshold) {
            a->threshold.rssi = c->threshold->rssi;
            a->threshold.authmode = c->threshold->authmode;
            a->threshold.rssi_5g_adjustment = c->threshold->rssi_5g_adjustment;
        }
        if (c->pmf_cfg) {
            a->pmf_cfg.capable = c->pmf_cfg->capable;
            a->pmf_cfg.required = c->pmf_cfg->required;
        }
        a->rm_enabled = BIT_OF(c->bitmask, STA_CFG_RM);
        a->btm_enabled = BIT_OF(c->bitmask, STA_CFG_BTM);
        a->mbo_enabled = BIT_OF(c->bitmask, STA_CFG_MBO);
        a->ft_enabled = BIT_OF(c->bitmask, STA_CFG_FT);
        a->owe_enabled = BIT_OF(c->bitmask, STA_CFG_OWE);
        a->transition_disable = BIT_OF(c->bitmask, STA_CFG_TRANSITION_DISABLE);
        a->sae_pwe_h2e = c->sae_pwe_h2e;
        a->sae_pk_mode = c->sae_pk_mode;
        a->failure_retry_cnt = c->failure_retry_cnt;
        a->he_dcm_set = BIT_OF(c->he_bitmask, STA_HE_DCM_SET);
        a->he_dcm_max_constellation_tx = (c->he_bitmask >> STA_HE_DCM_MAX_TX) & 3;
        a->he_dcm_max_constellation_rx = (c->he_bitmask >> STA_HE_DCM_MAX_RX) & 3;
        a->he_mcs9_enabled = BIT_OF(c->he_bitmask, STA_HE_MCS9);
        a->he_su_beamformee_disabled = BIT_OF(c->he_bitmask, STA_HE_SU_BMFEE_DISABLED);
        a->he_trig_su_bmforming_feedback_disabled = BIT_OF(c->he_bitmask, STA_HE_TRIG_SU_FB_DISABLED);
        a->he_trig_mu_bmforming_partial_feedback_disabled = BIT_OF(c->he_bitmask, STA_HE_TRIG_MU_FB_DISABLED);
        a->he_trig_cqi_feedback_disabled = BIT_OF(c->he_bitmask, STA_HE_TRIG_CQI_FB_DISABLED);
        a->vht_su_beamformee_disabled = BIT_OF(c->he_bitmask, STA_VHT_SU_BMFEE_DISABLED);
        a->vht_mu_beamformee_disabled = BIT_OF(c->he_bitmask, STA_VHT_MU_BMFEE_DISABLED);
        a->vht_mcs8_enabled = BIT_OF(c->he_bitmask, STA_VHT_MCS8);
        copy_bytes(a->sae_h2e_identifier, sizeof(a->sae_h2e_identifier), &c->sae_h2e_identifier);
    }
    rpc__free_unpacked(resp, NULL);
    return err;
}

esp_err_t esp_wifi_remote_scan_start(const wifi_scan_config_t *config, bool block) {
    RpcReqWifiScanStart m = RPC__REQ__WIFI_SCAN_START__INIT;
    WifiScanConfig sc = WIFI_SCAN_CONFIG__INIT;
    WifiScanTime st = WIFI_SCAN_TIME__INIT;
    WifiActiveScanTime at = WIFI_ACTIVE_SCAN_TIME__INIT;
    WifiScanChannelBitmap bm = WIFI_SCAN_CHANNEL_BITMAP__INIT;
    m.block = block;
    if (config) {
        if (config->ssid) {
            sc.ssid = (ProtobufCBinaryData){cstr_len(config->ssid, 32), config->ssid};
        }
        if (config->bssid) sc.bssid = (ProtobufCBinaryData){6, config->bssid};
        sc.channel = config->channel;
        sc.show_hidden = config->show_hidden;
        sc.scan_type = config->scan_type;
        at.min = config->scan_time.active.min;
        at.max = config->scan_time.active.max;
        st.active = &at;
        st.passive = config->scan_time.passive;
        sc.scan_time = &st;
        sc.home_chan_dwell_time = config->home_chan_dwell_time;
        bm.ghz_2_channels = config->channel_bitmap.ghz_2_channels;
        bm.ghz_5_channels = config->channel_bitmap.ghz_5_channels;
        sc.channel_bitmap = &bm;
        m.config = &sc;
        m.config_set = 1;
    }
    HOSTED_RPC_REQ(req, RPC_ID__Req_WifiScanStart, req_wifi_scan_start, &m);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_scan_start, block ? SCAN_BLOCK_TIMEOUT_MS : HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_scan_stop(void) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiScanStop);
    return HOSTED_RPC_CALL_STATUS(req, resp_wifi_scan_stop, HOSTED_RPC_TIMEOUT_MS);
}

esp_err_t esp_wifi_remote_scan_get_ap_num(uint16_t *number) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiScanGetApNum);
    esp_err_t err;
    Rpc *resp = hosted_rpc_call(&req, HOSTED_RPC_TIMEOUT_MS, &err);
    if (!resp) return err;
    err = HOSTED_RPC_STATUS(resp, resp_wifi_scan_get_ap_num);
    if (err == ESP_OK) *number = resp->resp_wifi_scan_get_ap_num->number;
    rpc__free_unpacked(resp, NULL);
    return err;
}

static void ap_record_from_rpc(wifi_ap_record_t *a, const WifiApRecord *r) {
    memset(a, 0, sizeof(*a));
    copy_bytes(a->bssid, sizeof(a->bssid), &r->bssid);
    copy_bytes(a->ssid, sizeof(a->ssid) - 1, &r->ssid);
    a->primary = r->primary;
    a->second = r->second;
    a->rssi = r->rssi;
    a->authmode = r->authmode;
    a->pairwise_cipher = r->pairwise_cipher;
    a->group_cipher = r->group_cipher;
    a->ant = r->ant;
    a->phy_11b = BIT_OF(r->bitmask, AP_REC_11B);
    a->phy_11g = BIT_OF(r->bitmask, AP_REC_11G);
    a->phy_11n = BIT_OF(r->bitmask, AP_REC_11N);
    a->phy_lr = BIT_OF(r->bitmask, AP_REC_LR);
    a->phy_11ax = BIT_OF(r->bitmask, AP_REC_11AX);
    a->wps = BIT_OF(r->bitmask, AP_REC_WPS);
    a->ftm_responder = BIT_OF(r->bitmask, AP_REC_FTM_RESPONDER);
    a->ftm_initiator = BIT_OF(r->bitmask, AP_REC_FTM_INITIATOR);
    a->phy_11a = BIT_OF(r->bitmask, AP_REC_11A);
    a->phy_11ac = BIT_OF(r->bitmask, AP_REC_11AC);
    if (r->country) {
        copy_bytes(a->country.cc, sizeof(a->country.cc), &r->country->cc);
        a->country.schan = r->country->schan;
        a->country.nchan = r->country->nchan;
        a->country.max_tx_power = r->country->max_tx_power;
        a->country.policy = r->country->policy;
    }
    if (r->he_ap) {
        a->he_ap.bss_color = r->he_ap->bitmask & 0x3f;
        a->he_ap.partial_bss_color = BIT_OF(r->he_ap->bitmask, 6);
        a->he_ap.bss_color_disabled = BIT_OF(r->he_ap->bitmask, 7);
        a->he_ap.bssid_index = r->he_ap->bssid_index;
    }
    a->bandwidth = r->bandwidth;
    a->vht_ch_freq1 = r->vht_ch_freq1;
    a->vht_ch_freq2 = r->vht_ch_freq2;
}

esp_err_t esp_wifi_remote_scan_get_ap_records(uint16_t *number, wifi_ap_record_t *ap_records) {
    RpcReqWifiScanGetApRecords m = RPC__REQ__WIFI_SCAN_GET_AP_RECORDS__INIT;
    m.number = *number;
    HOSTED_RPC_REQ(req, RPC_ID__Req_WifiScanGetApRecords, req_wifi_scan_get_ap_records, &m);
    esp_err_t err;
    Rpc *resp = hosted_rpc_call(&req, HOSTED_RPC_TIMEOUT_MS, &err);
    if (!resp) return err;
    err = HOSTED_RPC_STATUS(resp, resp_wifi_scan_get_ap_records);
    if (err == ESP_OK) {
        const RpcRespWifiScanGetApRecords *r = resp->resp_wifi_scan_get_ap_records;
        uint16_t n = r->n_ap_records < *number ? r->n_ap_records : *number;
        for (uint16_t i = 0; i < n; i++) ap_record_from_rpc(&ap_records[i], r->ap_records[i]);
        *number = n;
    }
    rpc__free_unpacked(resp, NULL);
    return err;
}

esp_err_t esp_wifi_remote_sta_get_ap_info(wifi_ap_record_t *ap_info) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiStaGetApInfo);
    esp_err_t err;
    Rpc *resp = hosted_rpc_call(&req, HOSTED_RPC_TIMEOUT_MS, &err);
    if (!resp) return err;
    err = HOSTED_RPC_STATUS(resp, resp_wifi_sta_get_ap_info);
    if (err == ESP_OK) {
        if (resp->resp_wifi_sta_get_ap_info->ap_record) {
            ap_record_from_rpc(ap_info, resp->resp_wifi_sta_get_ap_info->ap_record);
        } else {
            err = ESP_FAIL;
        }
    }
    rpc__free_unpacked(resp, NULL);
    return err;
}

esp_err_t esp_wifi_remote_sta_get_rssi(int *rssi) {
    HOSTED_RPC_REQ_NOARGS(req, RPC_ID__Req_WifiStaGetRssi);
    esp_err_t err;
    Rpc *resp = hosted_rpc_call(&req, HOSTED_RPC_TIMEOUT_MS, &err);
    if (!resp) return err;
    err = HOSTED_RPC_STATUS(resp, resp_wifi_sta_get_rssi);
    if (err == ESP_OK) *rssi = resp->resp_wifi_sta_get_rssi->rssi;
    rpc__free_unpacked(resp, NULL);
    return err;
}
