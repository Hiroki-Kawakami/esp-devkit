/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "wifi.h"

#include <stdlib.h>
#include <string.h>

#include "esp_event.h"
#include "esp_private/wifi.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/task.h"
#include "transport.h"

#define MAX_AP_RECORDS  64
#define RX_WAIT_MS      5
#define TX_RETRY_MS     100

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

#define EVT_DISCONNECTED BIT0


static bool s_inited;
static volatile bool s_connected;
static volatile bool s_swallow_disconnect;
static EventGroupHandle_t s_events;
static wifi_event_sta_connected_t s_last_connected;

static esp_err_t sta_rx(void *buffer, uint16_t len, void *eb) {
    if (transport_is_open()) {
        transport_send(HOSTED_IF_STA, 0, 0, buffer, len, pdMS_TO_TICKS(RX_WAIT_MS));
    }
    esp_wifi_internal_free_rx_buffer(eb);
    return ESP_OK;
}

/* Holding the SDIO buffer while the driver is out of TX buffers stalls the
 * host through the token count instead of dropping the frame. */
void wifi_on_host_packet(uint8_t *frame, uint16_t len) {
    for (int i = 0; s_connected && i < TX_RETRY_MS; i++) {
        if (esp_wifi_internal_tx(WIFI_IF_STA, frame, len) != ESP_ERR_NO_MEM) return;
        vTaskDelay(1);
    }
}

static void send_no_args(int32_t event_id) {
    RpcEventWifiEventNoArgs m = RPC__EVENT__WIFI_EVENT_NO_ARGS__INIT;
    m.event_id = event_id;
    RPC_EVENT(RPC_ID__Event_WifiEventNoArgs, event_wifi_event_no_args, &m);
}

static void send_connected(const wifi_event_sta_connected_t *e) {
    WifiEventStaConnected c = WIFI_EVENT_STA_CONNECTED__INIT;
    c.ssid = (ProtobufCBinaryData){sizeof(e->ssid), (uint8_t *)e->ssid};
    c.ssid_len = e->ssid_len;
    c.bssid = (ProtobufCBinaryData){sizeof(e->bssid), (uint8_t *)e->bssid};
    c.channel = e->channel;
    c.authmode = e->authmode;
    c.aid = e->aid;
    RpcEventStaConnected m = RPC__EVENT__STA_CONNECTED__INIT;
    m.sta_connected = &c;
    RPC_EVENT(RPC_ID__Event_StaConnected, event_sta_connected, &m);
}

static void on_wifi_event(void *arg, esp_event_base_t base, int32_t id, void *data) {
    switch (id) {
    case WIFI_EVENT_STA_START:
        break;
    case WIFI_EVENT_STA_CONNECTED:
        s_last_connected = *(wifi_event_sta_connected_t *)data;
        s_connected = true;
        esp_wifi_internal_reg_rxcb(WIFI_IF_STA, sta_rx);
        send_connected(&s_last_connected);
        break;
    case WIFI_EVENT_STA_DISCONNECTED: {
        s_connected = false;
        esp_wifi_internal_reg_rxcb(WIFI_IF_STA, NULL);
        if (s_swallow_disconnect) {
            s_swallow_disconnect = false;
            xEventGroupSetBits(s_events, EVT_DISCONNECTED);
            break;
        }
        const wifi_event_sta_disconnected_t *e = data;
        WifiEventStaDisconnected d = WIFI_EVENT_STA_DISCONNECTED__INIT;
        d.ssid = (ProtobufCBinaryData){sizeof(e->ssid), (uint8_t *)e->ssid};
        d.ssid_len = e->ssid_len;
        d.bssid = (ProtobufCBinaryData){sizeof(e->bssid), (uint8_t *)e->bssid};
        d.reason = e->reason;
        d.rssi = e->rssi;
        RpcEventStaDisconnected m = RPC__EVENT__STA_DISCONNECTED__INIT;
        m.sta_disconnected = &d;
        RPC_EVENT(RPC_ID__Event_StaDisconnected, event_sta_disconnected, &m);
        break;
    }
    case WIFI_EVENT_SCAN_DONE: {
        const wifi_event_sta_scan_done_t *e = data;
        WifiEventStaScanDone s = WIFI_EVENT_STA_SCAN_DONE__INIT;
        s.status = e->status;
        s.number = e->number;
        s.scan_id = e->scan_id;
        RpcEventStaScanDone m = RPC__EVENT__STA_SCAN_DONE__INIT;
        m.scan_done = &s;
        RPC_EVENT(RPC_ID__Event_StaScanDone, event_sta_scan_done, &m);
        break;
    }
    default:
        send_no_args(id);
        break;
    }
}

void wifi_rpc_init(const Rpc *req, Rpc *resp) {
    const RpcReqWifiInit *r = req->req_wifi_init;
    esp_err_t err = ESP_OK;
    if (!r || !r->cfg) {
        err = ESP_ERR_INVALID_ARG;
    } else if (!s_inited) {
        const WifiInitConfig *h = r->cfg;
        wifi_init_config_t cfg = WIFI_INIT_CONFIG_DEFAULT();
        cfg.static_rx_buf_num = h->static_rx_buf_num;
        cfg.dynamic_rx_buf_num = h->dynamic_rx_buf_num;
        cfg.tx_buf_type = h->tx_buf_type;
        cfg.static_tx_buf_num = h->static_tx_buf_num;
        cfg.dynamic_tx_buf_num = h->dynamic_tx_buf_num;
        cfg.rx_mgmt_buf_type = h->rx_mgmt_buf_type;
        cfg.rx_mgmt_buf_num = h->rx_mgmt_buf_num;
        /* The host's caps may enable a PSRAM TX cache this chip cannot back. */
        if (h->feature_caps == cfg.feature_caps) cfg.cache_tx_buf_num = h->cache_tx_buf_num;
        cfg.csi_enable = h->csi_enable;
        cfg.ampdu_rx_enable = h->ampdu_rx_enable;
        cfg.ampdu_tx_enable = h->ampdu_tx_enable;
        cfg.amsdu_tx_enable = h->amsdu_tx_enable;
        cfg.nvs_enable = h->nvs_enable;
        cfg.nano_enable = h->nano_enable;
        cfg.rx_ba_win = h->rx_ba_win;
        cfg.wifi_task_core_id = h->wifi_task_core_id;
        cfg.beacon_max_len = h->beacon_max_len;
        cfg.mgmt_sbuf_num = h->mgmt_sbuf_num;
        cfg.sta_disconnected_pm = h->sta_disconnected_pm;
        cfg.espnow_max_encrypt_num = h->espnow_max_encrypt_num;
        cfg.tx_hetb_queue_num = h->tx_hetb_queue_num;
        cfg.dump_hesigb_enable = h->dump_hesigb_enable;
        err = esp_wifi_init(&cfg);
        if (err == ESP_OK) {
            s_events = xEventGroupCreate();
            esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, on_wifi_event, NULL);
            s_inited = true;
        }
    }
    RPC_REPLY_STATUS(resp, RpcRespWifiInit, RPC__RESP__WIFI_INIT__INIT, resp_wifi_init, err);
}

void wifi_rpc_deinit(const Rpc *req, Rpc *resp) {
    esp_err_t err = esp_wifi_deinit();
    if (err == ESP_OK && s_inited) {
        esp_event_handler_unregister(WIFI_EVENT, ESP_EVENT_ANY_ID, on_wifi_event);
        vEventGroupDelete(s_events);
        s_inited = false;
        s_connected = false;
    }
    RPC_REPLY_STATUS(resp, RpcRespWifiDeinit, RPC__RESP__WIFI_DEINIT__INIT, resp_wifi_deinit, err);
}

void wifi_rpc_set_mode(const Rpc *req, Rpc *resp) {
    const RpcReqSetMode *r = req->req_set_wifi_mode;
    esp_err_t err = r ? esp_wifi_set_mode(r->mode) : ESP_ERR_INVALID_ARG;
    RPC_REPLY_STATUS(resp, RpcRespSetMode, RPC__RESP__SET_MODE__INIT, resp_set_wifi_mode, err);
}

void wifi_rpc_get_mode(const Rpc *req, Rpc *resp) {
    RpcRespGetMode m = RPC__RESP__GET_MODE__INIT;
    wifi_mode_t mode = WIFI_MODE_NULL;
    m.resp = esp_wifi_get_mode(&mode);
    m.mode = mode;
    RPC_REPLY(resp, resp_get_wifi_mode, &m);
}

void wifi_rpc_set_storage(const Rpc *req, Rpc *resp) {
    const RpcReqWifiSetStorage *r = req->req_wifi_set_storage;
    esp_err_t err = r ? esp_wifi_set_storage(r->storage) : ESP_ERR_INVALID_ARG;
    RPC_REPLY_STATUS(resp, RpcRespWifiSetStorage, RPC__RESP__WIFI_SET_STORAGE__INIT,
                     resp_wifi_set_storage, err);
}

void wifi_rpc_get_mac(const Rpc *req, Rpc *resp) {
    const RpcReqGetMacAddress *r = req->req_get_mac_address;
    RpcRespGetMacAddress m = RPC__RESP__GET_MAC_ADDRESS__INIT;
    uint8_t mac[6];
    m.resp = r ? esp_wifi_get_mac(r->mode, mac) : ESP_ERR_INVALID_ARG;
    if (m.resp == ESP_OK) m.mac = (ProtobufCBinaryData){sizeof(mac), mac};
    RPC_REPLY(resp, resp_get_mac_address, &m);
}

void wifi_rpc_set_mac(const Rpc *req, Rpc *resp) {
    const RpcReqSetMacAddress *r = req->req_set_mac_address;
    esp_err_t err = ESP_ERR_INVALID_ARG;
    if (r && r->mac.len == 6) err = esp_wifi_set_mac(r->mode, r->mac.data);
    RPC_REPLY_STATUS(resp, RpcRespSetMacAddress, RPC__RESP__SET_MAC_ADDRESS__INIT,
                     resp_set_mac_address, err);
}

void wifi_rpc_start(const Rpc *req, Rpc *resp) {
    esp_err_t err = esp_wifi_start();
    RPC_REPLY_STATUS(resp, RpcRespWifiStart, RPC__RESP__WIFI_START__INIT, resp_wifi_start, err);
    wifi_mode_t mode;
    if (err == ESP_OK && esp_wifi_get_mode(&mode) == ESP_OK &&
        (mode == WIFI_MODE_STA || mode == WIFI_MODE_APSTA)) {
        send_no_args(WIFI_EVENT_STA_START);
    }
}

void wifi_rpc_stop(const Rpc *req, Rpc *resp) {
    esp_err_t err = esp_wifi_stop();
    RPC_REPLY_STATUS(resp, RpcRespWifiStop, RPC__RESP__WIFI_STOP__INIT, resp_wifi_stop, err);
}

static void copy_str(uint8_t *dst, size_t cap, const ProtobufCBinaryData *src) {
    size_t n = src->len < cap ? src->len : cap;
    memcpy(dst, src->data, n);
    if (n < cap) dst[n] = 0;
}

static void sta_config_from_rpc(wifi_sta_config_t *a, const WifiStaConfig *c) {
    copy_str(a->ssid, sizeof(a->ssid), &c->ssid);
    copy_str(a->password, sizeof(a->password), &c->password);
    a->scan_method = c->scan_method;
    a->bssid_set = c->bssid_set;
    if (c->bssid_set && c->bssid.len == sizeof(a->bssid)) memcpy(a->bssid, c->bssid.data, sizeof(a->bssid));
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
    copy_str(a->sae_h2e_identifier, sizeof(a->sae_h2e_identifier), &c->sae_h2e_identifier);
}

void wifi_rpc_set_config(const Rpc *req, Rpc *resp) {
    const RpcReqWifiSetConfig *r = req->req_wifi_set_config;
    esp_err_t err;
    if (!r || !r->cfg) {
        err = ESP_ERR_INVALID_ARG;
    } else if (r->iface != WIFI_IF_STA || r->cfg->u_case != WIFI_CONFIG__U_STA) {
        err = ESP_ERR_NOT_SUPPORTED;
    } else {
        wifi_config_t cfg = {0};
        sta_config_from_rpc(&cfg.sta, r->cfg->sta);
        err = esp_wifi_set_config(WIFI_IF_STA, &cfg);
        if (err == ESP_ERR_WIFI_STATE) {
            /* Rejected while an association is in flight: end it quietly so the
             * host does not see a stray DISCONNECTED for its next attempt. */
            xEventGroupClearBits(s_events, EVT_DISCONNECTED);
            s_swallow_disconnect = true;
            if (esp_wifi_disconnect() == ESP_OK) {
                xEventGroupWaitBits(s_events, EVT_DISCONNECTED, pdTRUE, pdTRUE, pdMS_TO_TICKS(2000));
            }
            s_swallow_disconnect = false;
            err = esp_wifi_set_config(WIFI_IF_STA, &cfg);
        }
    }
    RPC_REPLY_STATUS(resp, RpcRespWifiSetConfig, RPC__RESP__WIFI_SET_CONFIG__INIT,
                     resp_wifi_set_config, err);
}

void wifi_rpc_get_config(const Rpc *req, Rpc *resp) {
    const RpcReqWifiGetConfig *r = req->req_wifi_get_config;
    RpcRespWifiGetConfig m = RPC__RESP__WIFI_GET_CONFIG__INIT;
    WifiConfig wc = WIFI_CONFIG__INIT;
    WifiStaConfig sc = WIFI_STA_CONFIG__INIT;
    WifiScanThreshold th = WIFI_SCAN_THRESHOLD__INIT;
    WifiPmfConfig pmf = WIFI_PMF_CONFIG__INIT;
    wifi_config_t cfg = {0};

    if (!r) {
        m.resp = ESP_ERR_INVALID_ARG;
    } else if (r->iface != WIFI_IF_STA) {
        m.resp = ESP_ERR_NOT_SUPPORTED;
    } else {
        m.resp = esp_wifi_get_config(WIFI_IF_STA, &cfg);
    }
    if (m.resp == ESP_OK) {
        const wifi_sta_config_t *a = &cfg.sta;
        sc.ssid = (ProtobufCBinaryData){strnlen((char *)a->ssid, sizeof(a->ssid)), (uint8_t *)a->ssid};
        sc.password = (ProtobufCBinaryData){strnlen((char *)a->password, sizeof(a->password)),
                                            (uint8_t *)a->password};
        sc.scan_method = a->scan_method;
        sc.bssid_set = a->bssid_set;
        sc.bssid = (ProtobufCBinaryData){sizeof(a->bssid), (uint8_t *)a->bssid};
        sc.channel = a->channel;
        sc.listen_interval = a->listen_interval;
        sc.sort_method = a->sort_method;
        th.rssi = a->threshold.rssi;
        th.authmode = a->threshold.authmode;
        th.rssi_5g_adjustment = a->threshold.rssi_5g_adjustment;
        sc.threshold = &th;
        pmf.capable = a->pmf_cfg.capable;
        pmf.required = a->pmf_cfg.required;
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
            strnlen((char *)a->sae_h2e_identifier, sizeof(a->sae_h2e_identifier)),
            (uint8_t *)a->sae_h2e_identifier};
        sc.sae_pk_mode = a->sae_pk_mode;
        wc.u_case = WIFI_CONFIG__U_STA;
        wc.sta = &sc;
        m.iface = WIFI_IF_STA;
        m.cfg = &wc;
    }
    RPC_REPLY(resp, resp_wifi_get_config, &m);
}

void wifi_rpc_connect(const Rpc *req, Rpc *resp) {
    wifi_config_t cfg;
    esp_err_t err = esp_wifi_get_config(WIFI_IF_STA, &cfg);
    bool resend = false;
    if (err == ESP_OK && cfg.sta.ssid[0] == 0) {
        err = ESP_ERR_WIFI_SSID;
    } else if (err == ESP_OK && s_connected) {
        resend = true;
    } else if (err == ESP_OK) {
        err = esp_wifi_connect();
        if (err == ESP_ERR_WIFI_CONN) err = ESP_OK;
    }
    RPC_REPLY_STATUS(resp, RpcRespWifiConnect, RPC__RESP__WIFI_CONNECT__INIT, resp_wifi_connect, err);
    if (resend) send_connected(&s_last_connected);
}

void wifi_rpc_disconnect(const Rpc *req, Rpc *resp) {
    esp_err_t err = esp_wifi_disconnect();
    RPC_REPLY_STATUS(resp, RpcRespWifiDisconnect, RPC__RESP__WIFI_DISCONNECT__INIT,
                     resp_wifi_disconnect, err);
}

void wifi_rpc_set_ps(const Rpc *req, Rpc *resp) {
    const RpcReqSetPs *r = req->req_wifi_set_ps;
    esp_err_t err = r ? esp_wifi_set_ps(r->type) : ESP_ERR_INVALID_ARG;
    RPC_REPLY_STATUS(resp, RpcRespSetPs, RPC__RESP__SET_PS__INIT, resp_wifi_set_ps, err);
}

void wifi_rpc_get_ps(const Rpc *req, Rpc *resp) {
    RpcRespGetPs m = RPC__RESP__GET_PS__INIT;
    wifi_ps_type_t type = WIFI_PS_NONE;
    m.resp = esp_wifi_get_ps(&type);
    m.type = type;
    RPC_REPLY(resp, resp_wifi_get_ps, &m);
}

void wifi_rpc_scan_start(const Rpc *req, Rpc *resp) {
    const RpcReqWifiScanStart *r = req->req_wifi_scan_start;
    esp_err_t err;
    if (!r) {
        err = ESP_ERR_INVALID_ARG;
    } else if (!r->config_set || !r->config) {
        err = esp_wifi_scan_start(NULL, r->block);
    } else {
        const WifiScanConfig *c = r->config;
        uint8_t ssid[33] = {0};
        uint8_t bssid[6];
        wifi_scan_config_t cfg = {
            .channel = c->channel,
            .show_hidden = c->show_hidden,
            .scan_type = c->scan_type,
            .home_chan_dwell_time = c->home_chan_dwell_time,
        };
        if (c->ssid.len) {
            copy_str(ssid, sizeof(ssid) - 1, &c->ssid);
            cfg.ssid = ssid;
        }
        if (c->bssid.len == sizeof(bssid)) {
            memcpy(bssid, c->bssid.data, sizeof(bssid));
            cfg.bssid = bssid;
        }
        if (c->scan_time) {
            if (c->scan_time->active) {
                cfg.scan_time.active.min = c->scan_time->active->min;
                cfg.scan_time.active.max = c->scan_time->active->max;
            }
            cfg.scan_time.passive = c->scan_time->passive;
        }
        if (c->channel_bitmap) {
            cfg.channel_bitmap.ghz_2_channels = c->channel_bitmap->ghz_2_channels;
            cfg.channel_bitmap.ghz_5_channels = c->channel_bitmap->ghz_5_channels;
        }
        err = esp_wifi_scan_start(&cfg, r->block);
    }
    RPC_REPLY_STATUS(resp, RpcRespWifiScanStart, RPC__RESP__WIFI_SCAN_START__INIT,
                     resp_wifi_scan_start, err);
}

void wifi_rpc_scan_stop(const Rpc *req, Rpc *resp) {
    esp_err_t err = esp_wifi_scan_stop();
    RPC_REPLY_STATUS(resp, RpcRespWifiScanStop, RPC__RESP__WIFI_SCAN_STOP__INIT,
                     resp_wifi_scan_stop, err);
}

void wifi_rpc_scan_get_ap_num(const Rpc *req, Rpc *resp) {
    RpcRespWifiScanGetApNum m = RPC__RESP__WIFI_SCAN_GET_AP_NUM__INIT;
    uint16_t n = 0;
    m.resp = esp_wifi_scan_get_ap_num(&n);
    m.number = n;
    RPC_REPLY(resp, resp_wifi_scan_get_ap_num, &m);
}

typedef struct {
    WifiApRecord rec;
    WifiCountry country;
    WifiHeApInfo he;
} ap_record_msg_t;

static void ap_record_to_rpc(const wifi_ap_record_t *a, ap_record_msg_t *m) {
    m->rec = (WifiApRecord)WIFI_AP_RECORD__INIT;
    m->country = (WifiCountry)WIFI_COUNTRY__INIT;
    m->he = (WifiHeApInfo)WIFI_HE_AP_INFO__INIT;
    WifiApRecord *r = &m->rec;
    size_t ssid_len = strnlen((char *)a->ssid, sizeof(a->ssid)) + 1;
    if (ssid_len > sizeof(a->ssid)) ssid_len = sizeof(a->ssid);
    r->bssid = (ProtobufCBinaryData){sizeof(a->bssid), (uint8_t *)a->bssid};
    r->ssid = (ProtobufCBinaryData){ssid_len, (uint8_t *)a->ssid};
    r->primary = a->primary;
    r->second = a->second;
    r->rssi = a->rssi;
    r->authmode = a->authmode;
    r->pairwise_cipher = a->pairwise_cipher;
    r->group_cipher = a->group_cipher;
    r->ant = a->ant;
    r->bitmask = a->phy_11b << AP_REC_11B | a->phy_11g << AP_REC_11G | a->phy_11n << AP_REC_11N |
                 a->phy_lr << AP_REC_LR | a->phy_11ax << AP_REC_11AX | a->wps << AP_REC_WPS |
                 a->ftm_responder << AP_REC_FTM_RESPONDER |
                 a->ftm_initiator << AP_REC_FTM_INITIATOR | a->phy_11a << AP_REC_11A |
                 a->phy_11ac << AP_REC_11AC;
    m->country.cc = (ProtobufCBinaryData){sizeof(a->country.cc), (uint8_t *)a->country.cc};
    m->country.schan = a->country.schan;
    m->country.nchan = a->country.nchan;
    m->country.max_tx_power = a->country.max_tx_power;
    m->country.policy = a->country.policy;
    r->country = &m->country;
    m->he.bitmask = a->he_ap.bss_color | a->he_ap.partial_bss_color << 6 |
                    a->he_ap.bss_color_disabled << 7;
    m->he.bssid_index = a->he_ap.bssid_index;
    r->he_ap = &m->he;
    r->bandwidth = a->bandwidth;
    r->vht_ch_freq1 = a->vht_ch_freq1;
    r->vht_ch_freq2 = a->vht_ch_freq2;
}

void wifi_rpc_scan_get_ap_records(const Rpc *req, Rpc *resp) {
    const RpcReqWifiScanGetApRecords *r = req->req_wifi_scan_get_ap_records;
    RpcRespWifiScanGetApRecords m = RPC__RESP__WIFI_SCAN_GET_AP_RECORDS__INIT;
    uint16_t n = r && r->number > 0 ? (r->number < MAX_AP_RECORDS ? r->number : MAX_AP_RECORDS) : 0;
    wifi_ap_record_t *recs = n ? calloc(n, sizeof(*recs)) : NULL;
    ap_record_msg_t *msgs = n ? calloc(n, sizeof(*msgs)) : NULL;
    WifiApRecord **ptrs = n ? calloc(n, sizeof(*ptrs)) : NULL;

    if (!r) {
        m.resp = ESP_ERR_INVALID_ARG;
    } else if (n && (!recs || !msgs || !ptrs)) {
        m.resp = ESP_ERR_NO_MEM;
    } else {
        m.resp = esp_wifi_scan_get_ap_records(&n, recs);
        if (m.resp == ESP_OK) {
            for (uint16_t i = 0; i < n; i++) {
                ap_record_to_rpc(&recs[i], &msgs[i]);
                ptrs[i] = &msgs[i].rec;
            }
            m.number = n;
            m.n_ap_records = n;
            m.ap_records = ptrs;
        }
    }
    RPC_REPLY(resp, resp_wifi_scan_get_ap_records, &m);
    free(ptrs);
    free(msgs);
    free(recs);
}

void wifi_rpc_sta_get_ap_info(const Rpc *req, Rpc *resp) {
    RpcRespWifiStaGetApInfo m = RPC__RESP__WIFI_STA_GET_AP_INFO__INIT;
    wifi_ap_record_t rec;
    ap_record_msg_t msg;
    m.resp = esp_wifi_sta_get_ap_info(&rec);
    if (m.resp == ESP_OK) {
        ap_record_to_rpc(&rec, &msg);
        m.ap_record = &msg.rec;
    }
    RPC_REPLY(resp, resp_wifi_sta_get_ap_info, &m);
}

void wifi_rpc_sta_get_rssi(const Rpc *req, Rpc *resp) {
    RpcRespWifiStaGetRssi m = RPC__RESP__WIFI_STA_GET_RSSI__INIT;
    int rssi = 0;
    m.resp = esp_wifi_sta_get_rssi(&rssi);
    m.rssi = rssi;
    RPC_REPLY(resp, resp_wifi_sta_get_rssi, &m);
}
