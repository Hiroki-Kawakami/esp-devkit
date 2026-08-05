/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP-IDF v6.0.2 esp_mac host implementation. The first process without an
 * injected SIMULATOR_BASE_MAC generates a local/unicast factory MAC and saves
 * it in idf_compat's JSON-backed NVS. Interface addresses follow the IDF
 * four-universal-address scheme and are cached once read.
 */

#include "esp_mac.h"
#include "esp_log.h"
#include "nvs.h"
#include "idf_compat_internal.h"

#include <pthread.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAC48_LEN 6
#define EUI64_LEN 8
#define MAC_EXT_LEN 2
#define FACTORY_MAC_KEY "factory_mac"

typedef struct {
    esp_mac_type_t type;
    size_t len;
    bool set;
    uint8_t value[EUI64_LEN];
} mac_entry_t;

static const char *TAG = "esp_mac";
static pthread_mutex_t s_mutex = PTHREAD_MUTEX_INITIALIZER;
static bool s_factory_init_attempted;
static esp_err_t s_factory_init_result = ESP_FAIL;

static mac_entry_t s_mac_table[] = {
    { .type = ESP_MAC_WIFI_STA,      .len = MAC48_LEN },
    { .type = ESP_MAC_WIFI_SOFTAP,   .len = MAC48_LEN },
    { .type = ESP_MAC_BT,            .len = MAC48_LEN },
    { .type = ESP_MAC_ETH,           .len = MAC48_LEN },
    { .type = ESP_MAC_IEEE802154,    .len = EUI64_LEN },
    { .type = ESP_MAC_BASE,          .len = MAC48_LEN },
    { .type = ESP_MAC_EFUSE_FACTORY, .len = MAC48_LEN },
    { .type = ESP_MAC_EFUSE_CUSTOM,  .len = MAC48_LEN },
    { .type = ESP_MAC_EFUSE_EXT,     .len = MAC_EXT_LEN },
};

static mac_entry_t *get_entry(esp_mac_type_t type) {
    for (size_t i = 0; i < sizeof(s_mac_table) / sizeof(s_mac_table[0]); i++) {
        if (s_mac_table[i].type == type) {
            return &s_mac_table[i];
        }
    }
    return NULL;
}

static bool is_unicast(const uint8_t *mac) {
    return (mac[0] & 0x01u) == 0;
}

static bool is_all_zero(const uint8_t *mac, size_t len) {
    for (size_t i = 0; i < len; i++) {
        if (mac[i] != 0) {
            return false;
        }
    }
    return true;
}

static int hex_digit(char c) {
    if (c >= '0' && c <= '9') {
        return c - '0';
    }
    if (c >= 'a' && c <= 'f') {
        return c - 'a' + 10;
    }
    if (c >= 'A' && c <= 'F') {
        return c - 'A' + 10;
    }
    return -1;
}

static esp_err_t parse_mac(const char *text, uint8_t *mac) {
    if (strlen(text) != 17) {
        return ESP_ERR_INVALID_MAC;
    }
    for (size_t i = 0; i < MAC48_LEN; i++) {
        size_t pos = i * 3;
        int high = hex_digit(text[pos]);
        int low = hex_digit(text[pos + 1]);
        if (high < 0 || low < 0 || (i < MAC48_LEN - 1 && text[pos + 2] != ':')) {
            return ESP_ERR_INVALID_MAC;
        }
        mac[i] = (uint8_t)((high << 4) | low);
    }
    if (!is_unicast(mac) || is_all_zero(mac, MAC48_LEN)) {
        return ESP_ERR_INVALID_MAC;
    }
    return ESP_OK;
}

static esp_err_t generate_factory_mac(uint8_t *mac) {
    FILE *random = fopen("/dev/urandom", "rb");
    if (!random) {
        return ESP_FAIL;
    }
    size_t read_len = fread(mac, 1, MAC48_LEN, random);
    fclose(random);
    if (read_len != MAC48_LEN) {
        return ESP_FAIL;
    }

    /* Set locally administered and clear multicast without embedding an
     * environment-specific address in the source. */
    mac[0] = (uint8_t)((mac[0] & 0xfcu) | 0x02u);
    return ESP_OK;
}

static esp_err_t load_or_create_factory_mac(uint8_t *mac) {
    nvs_handle_t handle;
    esp_err_t err = nvs_open(IDF_COMPAT_SIM_NVS_NAMESPACE, NVS_READWRITE, &handle);
    if (err != ESP_OK) {
        return ESP_FAIL;
    }

    size_t len = MAC48_LEN;
    err = nvs_get_blob(handle, FACTORY_MAC_KEY, mac, &len);
    if (err == ESP_ERR_NVS_NOT_FOUND) {
        err = generate_factory_mac(mac);
        if (err == ESP_OK) {
            err = nvs_set_blob(handle, FACTORY_MAC_KEY, mac, MAC48_LEN);
        }
        if (err == ESP_OK) {
            err = nvs_commit(handle);
        }
        nvs_close(handle);
        return err == ESP_OK ? ESP_OK : ESP_FAIL;
    }

    nvs_close(handle);
    if (err != ESP_OK || len != MAC48_LEN || !is_unicast(mac) || is_all_zero(mac, MAC48_LEN)) {
        return ESP_ERR_INVALID_MAC;
    }
    return ESP_OK;
}

static esp_err_t ensure_factory_mac_locked(void) {
    if (s_factory_init_attempted) {
        return s_factory_init_result;
    }
    s_factory_init_attempted = true;

    mac_entry_t *factory = get_entry(ESP_MAC_EFUSE_FACTORY);
    const char *configured = getenv("SIMULATOR_BASE_MAC");
    esp_err_t err = configured && configured[0] != '\0'
        ? parse_mac(configured, factory->value)
        : load_or_create_factory_mac(factory->value);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to initialize simulator factory MAC");
        s_factory_init_result = err;
        return err;
    }

    factory->set = true;
    mac_entry_t *base = get_entry(ESP_MAC_BASE);
    if (!base->set) {
        memcpy(base->value, factory->value, MAC48_LEN);
        base->set = true;
    }
    s_factory_init_result = ESP_OK;
    return ESP_OK;
}

static void derive_extension(const uint8_t *factory, uint8_t *extension) {
    /* A deterministic fold keeps the EUI-64 stable without storing another
     * identifier or embedding a fixed extension. */
    extension[0] = (uint8_t)(factory[0] ^ factory[2] ^ factory[4]);
    extension[1] = (uint8_t)(factory[1] ^ factory[3] ^ factory[5]);
}

static esp_err_t ensure_extension_locked(void) {
    mac_entry_t *extension = get_entry(ESP_MAC_EFUSE_EXT);
    if (extension->set) {
        return ESP_OK;
    }
    esp_err_t err = ensure_factory_mac_locked();
    if (err != ESP_OK) {
        return err;
    }
    derive_extension(get_entry(ESP_MAC_EFUSE_FACTORY)->value, extension->value);
    extension->set = true;
    return ESP_OK;
}

static esp_err_t ensure_base_locked(void) {
    mac_entry_t *base = get_entry(ESP_MAC_BASE);
    return base->set ? ESP_OK : ensure_factory_mac_locked();
}

static esp_err_t generate_interface_locked(mac_entry_t *entry) {
    esp_err_t err = ensure_base_locked();
    if (err != ESP_OK) {
        return err;
    }
    const uint8_t *base = get_entry(ESP_MAC_BASE)->value;

    switch (entry->type) {
        case ESP_MAC_WIFI_STA:
            memcpy(entry->value, base, MAC48_LEN);
            break;
        case ESP_MAC_WIFI_SOFTAP:
        case ESP_MAC_BT:
        case ESP_MAC_ETH: {
            static const uint8_t offsets[] = { 0, 1, 2, 3 };
            memcpy(entry->value, base, MAC48_LEN);
            entry->value[MAC48_LEN - 1] = (uint8_t)(
                entry->value[MAC48_LEN - 1] + offsets[entry->type]);
            break;
        }
        case ESP_MAC_IEEE802154:
            err = ensure_extension_locked();
            if (err != ESP_OK) {
                return err;
            }
            memcpy(entry->value, base, 3);
            memcpy(&entry->value[3], get_entry(ESP_MAC_EFUSE_EXT)->value, MAC_EXT_LEN);
            memcpy(&entry->value[5], &base[3], 3);
            break;
        default:
            return ESP_ERR_NOT_SUPPORTED;
    }
    entry->set = true;
    return ESP_OK;
}

esp_err_t esp_iface_mac_addr_set(const uint8_t *mac, esp_mac_type_t type) {
    if (!mac) {
        return ESP_ERR_INVALID_ARG;
    }
    mac_entry_t *entry = get_entry(type);
    if (!entry) {
        return ESP_ERR_NOT_SUPPORTED;
    }
    if (type == ESP_MAC_EFUSE_FACTORY || type == ESP_MAC_EFUSE_CUSTOM) {
        return ESP_ERR_INVALID_ARG;
    }
    if (type == ESP_MAC_BASE && !is_unicast(mac)) {
        return ESP_ERR_INVALID_ARG;
    }

    pthread_mutex_lock(&s_mutex);
    memcpy(entry->value, mac, entry->len);
    entry->set = true;
    pthread_mutex_unlock(&s_mutex);
    return ESP_OK;
}

esp_err_t esp_base_mac_addr_set(const uint8_t *mac) {
    return esp_iface_mac_addr_set(mac, ESP_MAC_BASE);
}

esp_err_t esp_read_mac(uint8_t *mac, esp_mac_type_t type) {
    if (!mac) {
        return ESP_ERR_INVALID_ARG;
    }
    mac_entry_t *entry = get_entry(type);
    if (!entry) {
        return ESP_ERR_NOT_SUPPORTED;
    }

    pthread_mutex_lock(&s_mutex);
    esp_err_t err = ESP_OK;
    if (!entry->set) {
        switch (type) {
            case ESP_MAC_BASE:
            case ESP_MAC_EFUSE_FACTORY:
                err = ensure_factory_mac_locked();
                break;
            case ESP_MAC_EFUSE_CUSTOM:
                err = ESP_ERR_INVALID_MAC;
                break;
            case ESP_MAC_EFUSE_EXT:
                err = ensure_extension_locked();
                break;
            default:
                err = generate_interface_locked(entry);
                break;
        }
    }
    if (err == ESP_OK) {
        memcpy(mac, entry->value, entry->len);
    }
    pthread_mutex_unlock(&s_mutex);
    return err;
}

esp_err_t esp_base_mac_addr_get(uint8_t *mac) {
    return esp_read_mac(mac, ESP_MAC_BASE);
}

esp_err_t esp_efuse_mac_get_default(uint8_t *mac) {
    return esp_read_mac(mac, ESP_MAC_EFUSE_FACTORY);
}

esp_err_t esp_efuse_mac_get_custom(uint8_t *mac) {
    if (!mac) {
        return ESP_ERR_INVALID_ARG;
    }
    return ESP_ERR_INVALID_MAC;
}

esp_err_t esp_derive_local_mac(uint8_t *local_mac, const uint8_t *universal_mac) {
    if (!local_mac || !universal_mac) {
        return ESP_ERR_INVALID_ARG;
    }
    memcpy(local_mac, universal_mac, MAC48_LEN);
    local_mac[0] |= 0x02u;
    if (local_mac[0] == universal_mac[0]) {
        local_mac[0] ^= 0x04u;
    }
    return ESP_OK;
}

size_t esp_mac_addr_len_get(esp_mac_type_t type) {
    mac_entry_t *entry = get_entry(type);
    return entry ? entry->len : 0;
}
