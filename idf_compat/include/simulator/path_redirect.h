/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t sim_path_redirect_add(const char *mount_point, const char *host_dir);
esp_err_t sim_path_redirect_remove(const char *mount_point);

#ifdef __cplusplus
}
#endif
