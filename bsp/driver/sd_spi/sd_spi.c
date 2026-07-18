/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "sd_spi.h"
#include <stdlib.h>
#include <string.h>
#include "driver/sdspi_host.h"
#include "esp_log.h"
#include "esp_vfs.h"
#include "esp_vfs_fat.h"
#include "sdmmc_cmd.h"

static const char *TAG = "sd_spi";

typedef struct {
    bsp_sd_t base;
    sd_spi_config_t config;
    sdmmc_card_t *card;
    char mount_point[ESP_VFS_PATH_MAX + 1];
    bool bus_initialized;
} sd_spi_t;

static esp_err_t sd_spi_mount(bsp_sd_t *self, const char *mount_point,
                              const bsp_sd_mount_config_t *config) {
    sd_spi_t *sd = (sd_spi_t *)self;
    size_t mount_point_len = strlen(mount_point);
    if (mount_point_len > ESP_VFS_PATH_MAX) return ESP_ERR_INVALID_ARG;
    if (sd->card) return ESP_ERR_INVALID_STATE;

    const bsp_sd_mount_config_t defaults = {0};
    if (!config) config = &defaults;
    if (config->max_freq_khz < 0) return ESP_ERR_INVALID_ARG;

    if (sd->config.bus_lifecycle == SD_SPI_BUS_MANAGED) {
        esp_err_t err = spi_bus_initialize(sd->config.spi_host, &sd->config.bus_config,
                                           sd->config.dma_channel);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "spi_bus_initialize: %s", esp_err_to_name(err));
            return err;
        }
        sd->bus_initialized = true;
    }

    sdmmc_host_t host = SDSPI_HOST_DEFAULT();
    host.slot = sd->config.spi_host;
    host.max_freq_khz = config->max_freq_khz > 0
                        ? config->max_freq_khz : SDMMC_FREQ_DEFAULT;

    sdspi_device_config_t device_config = SDSPI_DEVICE_CONFIG_DEFAULT();
    device_config.host_id = sd->config.spi_host;
    device_config.gpio_cs = sd->config.cs_io;

    const esp_vfs_fat_sdmmc_mount_config_t mount_config = {
        .format_if_mount_failed = config->format_if_mount_failed,
        .max_files = config->max_files > 0 ? config->max_files : 5,
        .allocation_unit_size = 0,
    };

    esp_err_t err = esp_vfs_fat_sdspi_mount(mount_point, &host, &device_config,
                                             &mount_config, &sd->card);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "esp_vfs_fat_sdspi_mount: %s", esp_err_to_name(err));
        if (sd->bus_initialized) {
            spi_bus_free(sd->config.spi_host);
            sd->bus_initialized = false;
        }
        sd->card = NULL;
        return err;
    }

    memcpy(sd->mount_point, mount_point, mount_point_len + 1);
    return ESP_OK;
}

static esp_err_t sd_spi_unmount(bsp_sd_t *self) {
    sd_spi_t *sd = (sd_spi_t *)self;
    if (!sd->card) return ESP_ERR_INVALID_STATE;

    esp_err_t err = esp_vfs_fat_sdcard_unmount(sd->mount_point, sd->card);
    sd->card = NULL;
    sd->mount_point[0] = '\0';

    if (sd->bus_initialized) {
        esp_err_t bus_err = spi_bus_free(sd->config.spi_host);
        sd->bus_initialized = false;
        if (err == ESP_OK) err = bus_err;
    }
    return err;
}

static bool sd_spi_is_mounted(bsp_sd_t *self) {
    return ((sd_spi_t *)self)->card != NULL;
}

static esp_err_t sd_spi_deinit(bsp_sd_t *self) {
    sd_spi_t *sd = (sd_spi_t *)self;
    esp_err_t err = ESP_OK;
    if (sd->card) err = sd_spi_unmount(self);
    free(sd);
    return err;
}

esp_err_t sd_spi_create(const sd_spi_config_t *config, bsp_sd_t **out_sd) {
    if (!config || !out_sd || config->cs_io == GPIO_NUM_NC) return ESP_ERR_INVALID_ARG;
    if (config->bus_lifecycle != SD_SPI_BUS_BORROWED &&
        config->bus_lifecycle != SD_SPI_BUS_MANAGED) return ESP_ERR_INVALID_ARG;

    sd_spi_t *sd = calloc(1, sizeof(*sd));
    if (!sd) return ESP_ERR_NO_MEM;
    sd->config = *config;
    sd->base.mount = sd_spi_mount;
    sd->base.unmount = sd_spi_unmount;
    sd->base.is_mounted = sd_spi_is_mounted;
    sd->base.deinit = sd_spi_deinit;
    *out_sd = &sd->base;
    return ESP_OK;
}
