/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "c6_flash.hpp"

#include <algorithm>
#include <cstring>

#include "driver/gpio.h"
#include "esp_attr.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "harness.h"
#include "mbedtls/base64.h"
#include "sdkconfig.h"

#if CONFIG_ESP_HOSTED_ENHANCED_HOST
#include "hosted_host.h"
#else
#include "esp_hosted.h"
#include "esp_hosted_ota.h"
#endif

namespace {

#if CONFIG_ESP_HOSTED_ENHANCED_HOST
esp_err_t connect() { return hosted_host_connect(); }
esp_err_t ota_begin() { return hosted_host_ota_begin(); }
esp_err_t ota_write(const uint8_t* data, size_t len) { return hosted_host_ota_write(data, len); }
esp_err_t ota_end() { return hosted_host_ota_end(); }
esp_err_t ota_activate() { return hosted_host_ota_activate(); }
esp_err_t fw_version(uint32_t* major, uint32_t* minor, uint32_t* patch) {
    return hosted_host_fw_version(major, minor, patch);
}
#else
esp_err_t connect() { return static_cast<esp_err_t>(esp_hosted_connect_to_slave()); }
esp_err_t ota_begin() { return esp_hosted_slave_ota_begin(); }
esp_err_t ota_write(const uint8_t* data, size_t len) {
    return esp_hosted_slave_ota_write(const_cast<uint8_t*>(data), len);
}
esp_err_t ota_end() { return esp_hosted_slave_ota_end(); }
esp_err_t ota_activate() { return esp_hosted_slave_ota_activate(); }
esp_err_t fw_version(uint32_t* major, uint32_t* minor, uint32_t* patch) {
    esp_hosted_coprocessor_fwver_t ver = {};
    esp_err_t err = static_cast<esp_err_t>(esp_hosted_get_coprocessor_fwversion(&ver));
    *major = ver.major1;
    *minor = ver.minor1;
    *patch = ver.patch1;
    return err;
}
#endif

const char* TAG = "c6_flash";
constexpr uint32_t kRecoveryMagic = 0x52435659;
constexpr size_t kChunk = 4096;
/* 1.x coprocessors never answer a larger OTAWrite, have no OTAActivate and
 * switch the boot slot on OTAEnd. */
constexpr size_t kLegacyChunk = 1400;
constexpr gpio_num_t kRecoveryPin = static_cast<gpio_num_t>(CONFIG_DEV_C6_RECOVERY_GPIO);

RTC_NOINIT_ATTR uint32_t s_recovery;
uint8_t s_chunk[kChunk];
size_t s_fill;
bool s_legacy;

void hold_low(bool low) {
    if (low) {
        gpio_set_direction(kRecoveryPin, GPIO_MODE_OUTPUT);
        gpio_set_level(kRecoveryPin, 0);
    } else {
        gpio_reset_pin(kRecoveryPin);
    }
}

void set_mode(uint32_t mode) {
    s_recovery = mode;
    hold_low(mode == kRecoveryMagic);
}

bool reply_err(const char* what, esp_err_t err) {
    if (err == ESP_OK) return true;
    harness_reply("ERR c6 %s: %s", what, esp_err_to_name(err));
    return true;
}

bool flush() {
    if (s_fill == 0) return true;
    esp_err_t err = ota_write(s_chunk, s_fill);
    s_fill = 0;
    return err == ESP_OK || !reply_err("write", err);
}

bool cmd_c6(int argc, const char* const* argv, void*) {
    if (argc < 2) return false;
    const char* sub = argv[1];

    if (!strcmp(sub, "recovery")) {
        set_mode(kRecoveryMagic);
    } else if (!strcmp(sub, "normal")) {
        set_mode(0);
    } else if (!strcmp(sub, "state")) {
        harness_reply("OK c6 %s", s_recovery == kRecoveryMagic ? "recovery" : "normal");
    } else if (!strcmp(sub, "pin") && argc == 3) {
        hold_low(!strcmp(argv[2], "low"));
    } else if (!strcmp(sub, "begin")) {
        s_fill = 0;
        uint32_t major = 0, minor = 0, patch = 0;
        s_legacy = fw_version(&major, &minor, &patch) == ESP_OK && major == 1;
        return reply_err("begin", ota_begin());
    } else if (!strcmp(sub, "data") && argc == 3) {
        size_t n = 0;
        uint8_t bin[512];
        if (mbedtls_base64_decode(bin, sizeof(bin), &n,
                                  reinterpret_cast<const unsigned char*>(argv[2]),
                                  strlen(argv[2])) != 0) {
            return false;
        }
        for (size_t off = 0; off < n;) {
            const size_t chunk = s_legacy ? kLegacyChunk : kChunk;
            size_t take = std::min(n - off, chunk - s_fill);
            memcpy(s_chunk + s_fill, bin + off, take);
            s_fill += take;
            off += take;
            if (s_fill == chunk && !flush()) return true;
        }
    } else if (!strcmp(sub, "end")) {
        if (!flush()) return true;
        return reply_err("end", ota_end());
    } else if (!strcmp(sub, "activate")) {
        esp_err_t err = s_legacy ? ESP_OK : ota_activate();
        if (err == ESP_OK) hold_low(false);
        return reply_err("activate", err);
    } else if (!strcmp(sub, "ver")) {
        uint32_t major = 0, minor = 0, patch = 0;
        esp_err_t err = fw_version(&major, &minor, &patch);
        if (err != ESP_OK) return reply_err("ver", err);
        harness_reply("OK c6 %u.%u.%u", (unsigned)major, (unsigned)minor, (unsigned)patch);
    } else {
        return false;
    }
    return true;
}

}  // namespace

bool c6_flash_start() {
    harness_register("c6", cmd_c6, nullptr);
    if (s_recovery != kRecoveryMagic) {
        s_recovery = 0;
        return false;
    }
    ESP_LOGW(TAG, "recovery: holding IO2 low");
    hold_low(true);
    connect();
    return true;
}
