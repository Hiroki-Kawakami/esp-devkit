/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Minimal Tab5 sample: brings up the BSP + LVGL and shows a centered label.
 */

#include "tab5_hello.hpp"
#include "display_manager.hpp"
#include "lvgl.hpp"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "sdkconfig.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>
#if CONFIG_HARNESS
#include "harness.h"
#endif
#include "screens/home_screen.hpp"

static const char *TAG = "tab5_hello";

static lv_display_t *s_display = nullptr;

static void lvgl_init() {
    lvgl_port_cfg_t config = {
        .task_priority    = 4,
        .task_stack       = 8192,
        .task_affinity    = 1,
        .task_max_sleep_ms = 500,
        .task_stack_caps  = MALLOC_CAP_INTERNAL | MALLOC_CAP_DEFAULT,
        .timer_period_ms  = 5,
    };
    esp_err_t err = lvgl_port_init(&config);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "lvgl_port_init: %s", esp_err_to_name(err));
        return;
    }

    bsp_size_t panel = bsp_display_get_size();
    size_t long_edge = panel.width > panel.height ? panel.width : panel.height;
    size_t buffer_size = long_edge * 24 *
        bsp_pixel_format_bytes(bsp_display_get_pixel_format());

    DisplayManagerConfig display_config = {};
    display_config.render_mode = DisplayRenderMode::Partial;
    display_config.buffer.buffer_size = buffer_size;
    for (auto &buffer : display_config.buffer.buffers) {
        buffer = heap_caps_aligned_alloc(64, buffer_size,
                                         MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL);
        if (!buffer) {
            ESP_LOGE(TAG, "partial buffer: no internal memory for %u bytes",
                     (unsigned)buffer_size);
            return;
        }
    }
    err = display_manager.create_display(display_config, &s_display);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "create_display: %s", esp_err_to_name(err));
        return;
    }
}

static const char *orientation_name(bsp_imu_orientation_t orientation) {
    switch (orientation) {
        case BSP_IMU_ORIENTATION_ROTATION_0:   return "ROTATION_0";
        case BSP_IMU_ORIENTATION_ROTATION_90:  return "ROTATION_90";
        case BSP_IMU_ORIENTATION_ROTATION_180: return "ROTATION_180";
        case BSP_IMU_ORIENTATION_ROTATION_270: return "ROTATION_270";
        case BSP_IMU_ORIENTATION_FACE_UP:      return "FACE_UP";
        case BSP_IMU_ORIENTATION_FACE_DOWN:    return "FACE_DOWN";
        default:                               return "UNKNOWN";
    }
}

static void imu_orientation_init() {
    bsp_imu_set_orientation_cb([](bsp_imu_orientation_t orientation, void *) {
        bsp_imu_sample_t sample = {};
        if (bsp_imu_read(&sample) != ESP_OK) {
            ESP_LOGI(TAG, "orientation: %s", orientation_name(orientation));
            return;
        }
        ESP_LOGI(TAG, "orientation: %s accel=(%.2f, %.2f, %.2f)", orientation_name(orientation),
                 sample.accel.x, sample.accel.y, sample.accel.z);

        if (orientation != BSP_IMU_ORIENTATION_ROTATION_0 &&
            orientation != BSP_IMU_ORIENTATION_ROTATION_90 &&
            orientation != BSP_IMU_ORIENTATION_ROTATION_180 &&
            orientation != BSP_IMU_ORIENTATION_ROTATION_270) {
            return;
        }
        auto rotation = static_cast<bsp_rotation_t>(orientation);
        lv_async_call([rotation] {
            esp_err_t err = display_manager.set_rotation(s_display, rotation);
            if (err != ESP_OK) {
                ESP_LOGW(TAG, "set_rotation: %s", esp_err_to_name(err));
            }
        });
    }, nullptr);
    esp_err_t err = bsp_imu_set_orientation_enabled(true);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "imu orientation: %s", esp_err_to_name(err));
    }
}

static bool s_rtc_boot_expired = false;

static void rtc_boot_check() {
    if (!bsp_rtc_is_available()) {
        ESP_LOGW(TAG, "rtc: not available");
        return;
    }
    bool valid = false;
    bsp_rtc_datetime_t dt = {};
    esp_err_t err = bsp_rtc_time_is_valid(&valid);
    if (err == ESP_OK) err = bsp_rtc_get_time(&dt);
    if (err == ESP_OK) err = bsp_rtc_timer_is_expired(&s_rtc_boot_expired);
    if (err != ESP_OK && err != ESP_ERR_NOT_SUPPORTED) {
        ESP_LOGE(TAG, "rtc: %s", esp_err_to_name(err));
        return;
    }
    ESP_LOGI(TAG, "rtc: %04u-%02u-%02u %02u:%02u:%02u wd=%u valid=%d timer_expired=%d",
             dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, dt.weekday,
             valid, s_rtc_boot_expired);
    bsp_rtc_timer_stop();
}

#if CONFIG_HARNESS
static bool rtc_reply_err(const char *what, esp_err_t err) {
    if (err == ESP_OK) return true;
    harness_reply("ERR rtc %s: %s", what, esp_err_to_name(err));
    return true;
}

static bool cmd_rtc(int argc, const char *const *argv, void *) {
    const char *sub = argc >= 2 ? argv[1] : "get";
    if (!strcmp(sub, "get")) {
        bool valid = false, expired = false;
        bsp_rtc_datetime_t dt = {};
        esp_err_t err = bsp_rtc_time_is_valid(&valid);
        if (err == ESP_OK) err = bsp_rtc_get_time(&dt);
        if (err == ESP_OK) err = bsp_rtc_timer_is_expired(&expired);
        if (err != ESP_OK && err != ESP_ERR_NOT_SUPPORTED) return rtc_reply_err("get", err);
        harness_reply("OK rtc %04u-%02u-%02u %02u:%02u:%02u wd=%u valid=%d expired=%d boot_expired=%d",
                      dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, dt.weekday,
                      valid, expired, s_rtc_boot_expired);
    } else if (!strcmp(sub, "set") && argc == 5) {
        unsigned year, month, day, hour, minute, second;
        if (sscanf(argv[2], "%u-%u-%u", &year, &month, &day) != 3 ||
            sscanf(argv[3], "%u:%u:%u", &hour, &minute, &second) != 3) {
            return false;
        }
        bsp_rtc_datetime_t dt = {
            .year    = (uint16_t)year,
            .month   = (uint8_t)month,
            .day     = (uint8_t)day,
            .hour    = (uint8_t)hour,
            .minute  = (uint8_t)minute,
            .second  = (uint8_t)second,
            .weekday = (uint8_t)atoi(argv[4]),
        };
        return rtc_reply_err("set", bsp_rtc_set_time(&dt));
    } else if (!strcmp(sub, "timer") && argc >= 3) {
        bool repeat = argc >= 4 && !strcmp(argv[3], "repeat");
        return rtc_reply_err("timer", bsp_rtc_timer_start((uint32_t)strtoul(argv[2], nullptr, 0), repeat));
    } else if (!strcmp(sub, "stop")) {
        return rtc_reply_err("stop", bsp_rtc_timer_stop());
    } else if (!strcmp(sub, "clear")) {
        return rtc_reply_err("clear", bsp_rtc_timer_clear());
    } else if (!strcmp(sub, "hwreset")) {
        harness_reply("OK rtc hwreset");
        return rtc_reply_err("hwreset", bsp_power_hw_reset());
    } else {
        return false;
    }
    return true;
}
#endif

void app_entry() {
    bsp_config_t bsp_config = {};
    bsp_config.display.pixel_format = BSP_PIXEL_FORMAT_RGB565;
    bsp_config.dispatch.task_priority = 6;
    bsp_config.dispatch.task_affinity = 1;
    bsp_config.keyboard.detect = BSP_KEYBOARD_DETECT_AUTO;
    bsp_init(&bsp_config);
    rtc_boot_check();
#if CONFIG_HARNESS
    harness_register("rtc", cmd_rtc, nullptr);
#endif
    lvgl_init();
    imu_orientation_init();

    lv_async_call([] {
        screen_manager.load(std::make_shared<HomeScreen>());
        bsp_display_set_brightness(80);
    });
}
