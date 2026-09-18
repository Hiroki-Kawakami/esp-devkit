/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host-side bsp_sd: "mounting" maps the mount point onto a host directory
 * (SIMULATOR_SDCARD_PATH env var, default simulator/sdcard relative to the
 * cwd — run.sh runs from the repo root).
 */

#include "bsp.h"
#include "simulator/path_redirect.h"

#include <stdlib.h>
#include <string.h>

#define MOUNT_POINT_MAX 64

static char g_mount_point[MOUNT_POINT_MAX];
static bool g_mounted;

esp_err_t bsp_sd_mount(const char *mount_point, const bsp_sd_mount_config_t *config) {
    (void)config;
    if (!mount_point || strlen(mount_point) >= MOUNT_POINT_MAX) return ESP_ERR_INVALID_ARG;
    if (g_mounted) return ESP_ERR_INVALID_STATE;
    const char *root = getenv("SIMULATOR_SDCARD_PATH");
    if (!root) root = "simulator/sdcard";
    esp_err_t err = sim_path_redirect_add(mount_point, root);
    if (err != ESP_OK) return err;
    strcpy(g_mount_point, mount_point);
    g_mounted = true;
    return ESP_OK;
}

esp_err_t bsp_sd_unmount(void) {
    if (!g_mounted) return ESP_ERR_INVALID_STATE;
    sim_path_redirect_remove(g_mount_point);
    g_mounted = false;
    return ESP_OK;
}

bool bsp_sd_is_mounted(void) {
    return g_mounted;
}
