/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sd_mmc.h"
#include <stdlib.h>
#include <string.h>
#include "esp_log.h"
#include "esp_vfs.h"
#include "esp_vfs_fat.h"

static const char *TAG = "sd_mmc";

typedef struct {
    bsp_sd_t base;
    sd_mmc_config_t config;
    sdmmc_card_t *card;
    char mount_point[ESP_VFS_PATH_MAX + 1];
    sd_pwr_ctrl_handle_t power;
    bool power_acquired;
} sd_mmc_t;

static esp_err_t sd_mmc_use_borrowed_host(void) {
    sdmmc_host_state_t state = {0};
    esp_err_t err = sdmmc_host_get_state(&state);
    if (err != ESP_OK) return err;
    if (!state.host_initialized) {
        ESP_LOGE(TAG, "borrowed SDMMC host is not initialized");
        return ESP_ERR_INVALID_STATE;
    }
    return ESP_OK;
}

static esp_err_t sd_mmc_mount(bsp_sd_t *self, const char *mount_point,
                              const bsp_sd_mount_config_t *config) {
    sd_mmc_t *sd = (sd_mmc_t *)self;
    size_t mount_point_len = strlen(mount_point);
    if (mount_point_len > ESP_VFS_PATH_MAX) return ESP_ERR_INVALID_ARG;
    if (sd->card) return ESP_ERR_INVALID_STATE;

    const bsp_sd_mount_config_t defaults = {0};
    if (!config) config = &defaults;
    if (config->max_freq_khz < 0) return ESP_ERR_INVALID_ARG;

    if (!sd->power_acquired && sd->config.power_acquire) {
        esp_err_t err = sd->config.power_acquire(sd->config.power_context,
                                                  &sd->power);
        if (err != ESP_OK) return err;
        sd->power_acquired = true;
    }

    sdmmc_host_t host = sd->config.host;
    if (sd->config.host_lifecycle == SD_MMC_HOST_BORROWED) {
        host.init = sd_mmc_use_borrowed_host;
    }
    if (config->max_freq_khz > 0) host.max_freq_khz = config->max_freq_khz;
    if (sd->power_acquired) host.pwr_ctrl_handle = sd->power;

    const esp_vfs_fat_sdmmc_mount_config_t mount_config = {
        .format_if_mount_failed = config->format_if_mount_failed,
        .max_files = config->max_files > 0 ? config->max_files : 5,
        .allocation_unit_size = sd->config.allocation_unit_size,
    };

    esp_err_t err = esp_vfs_fat_sdmmc_mount(mount_point, &host,
                                             &sd->config.slot_config,
                                             &mount_config, &sd->card);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "esp_vfs_fat_sdmmc_mount: %s", esp_err_to_name(err));
        sd->card = NULL;
        return err;
    }

    memcpy(sd->mount_point, mount_point, mount_point_len + 1);
    return ESP_OK;
}

static esp_err_t sd_mmc_unmount(bsp_sd_t *self) {
    sd_mmc_t *sd = (sd_mmc_t *)self;
    if (!sd->card) return ESP_ERR_INVALID_STATE;

    esp_err_t err = esp_vfs_fat_sdcard_unmount(sd->mount_point, sd->card);
    sd->card = NULL;
    sd->mount_point[0] = '\0';
    return err;
}

static bool sd_mmc_is_mounted(bsp_sd_t *self) {
    return ((sd_mmc_t *)self)->card != NULL;
}

static esp_err_t sd_mmc_deinit(bsp_sd_t *self) {
    sd_mmc_t *sd = (sd_mmc_t *)self;
    esp_err_t err = ESP_OK;
    if (sd->card) err = sd_mmc_unmount(self);
    if (sd->power_acquired) {
        esp_err_t power_err = sd->config.power_release(sd->config.power_context,
                                                        sd->power);
        if (err == ESP_OK) err = power_err;
    }
    free(sd);
    return err;
}

esp_err_t sd_mmc_create(const sd_mmc_config_t *config, bsp_sd_t **out_sd) {
    if (!config || !out_sd || !config->host.init) return ESP_ERR_INVALID_ARG;
    if (config->host_lifecycle != SD_MMC_HOST_BORROWED &&
        config->host_lifecycle != SD_MMC_HOST_MANAGED) return ESP_ERR_INVALID_ARG;
    if (!!config->power_acquire != !!config->power_release) return ESP_ERR_INVALID_ARG;

    sd_mmc_t *sd = calloc(1, sizeof(*sd));
    if (!sd) return ESP_ERR_NO_MEM;
    sd->config = *config;
    sd->base.mount = sd_mmc_mount;
    sd->base.unmount = sd_mmc_unmount;
    sd->base.is_mounted = sd_mmc_is_mounted;
    sd->base.deinit = sd_mmc_deinit;
    *out_sd = &sd->base;
    return ESP_OK;
}
