/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * ESP-IDF v6.1 esp_app_desc host implementation. PROJECT_VER and PROJECT_NAME
 * are injected by devkit_simulator().
 */

#include "esp_app_desc.h"

static const esp_app_desc_t s_app_desc = {
    .magic_word = ESP_APP_DESC_MAGIC_WORD,
    .version = PROJECT_VER,
    .project_name = PROJECT_NAME,
    .time = __TIME__,
    .date = __DATE__,
    .idf_ver = "simulator",
};

const esp_app_desc_t *esp_app_get_description(void) {
    return &s_app_desc;
}
