/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * M5PaperS3 SD card: FAT over the SDSPI host on the dedicated SPI2 bus.
 */

#include "bsp.h"
#include "esp_vfs.h"
#include "esp_vfs_fat.h"
#include "sdmmc_cmd.h"
#include "driver/sdspi_host.h"
#include "driver/spi_common.h"
#include "esp_log.h"
#include <string.h>

static const char *TAG = "paper_s3_sd";

#define PAPER_S3_SD_SPI_HOST      SPI2_HOST
#define PAPER_S3_SD_PIN_SCK       GPIO_NUM_39
#define PAPER_S3_SD_PIN_MOSI      GPIO_NUM_38
#define PAPER_S3_SD_PIN_MISO      GPIO_NUM_40
#define PAPER_S3_SD_PIN_CS        GPIO_NUM_47

static sdmmc_card_t *s_card;
static char          s_mount_point[ESP_VFS_PATH_MAX + 1];

esp_err_t bsp_sd_mount(const char *mount_point, const bsp_sd_mount_config_t *config) {
    if (!mount_point) return ESP_ERR_INVALID_ARG;
    if (s_card) return ESP_ERR_INVALID_STATE;

    const bsp_sd_mount_config_t defaults = {0};
    if (!config) config = &defaults;

    const spi_bus_config_t bus_cfg = {
        .mosi_io_num     = PAPER_S3_SD_PIN_MOSI,
        .miso_io_num     = PAPER_S3_SD_PIN_MISO,
        .sclk_io_num     = PAPER_S3_SD_PIN_SCK,
        .quadwp_io_num   = GPIO_NUM_NC,
        .quadhd_io_num   = GPIO_NUM_NC,
        .max_transfer_sz = 4000,
    };
    esp_err_t err = spi_bus_initialize(PAPER_S3_SD_SPI_HOST, &bus_cfg, SDSPI_DEFAULT_DMA);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "spi_bus_initialize: %s", esp_err_to_name(err));
        return err;
    }

    sdmmc_host_t host = SDSPI_HOST_DEFAULT();
    host.slot = PAPER_S3_SD_SPI_HOST;
    host.max_freq_khz = config->max_freq_khz > 0 ? config->max_freq_khz : SDMMC_FREQ_HIGHSPEED;

    sdspi_device_config_t dev_cfg = SDSPI_DEVICE_CONFIG_DEFAULT();
    dev_cfg.gpio_cs = PAPER_S3_SD_PIN_CS;
    dev_cfg.host_id = PAPER_S3_SD_SPI_HOST;

    const esp_vfs_fat_sdmmc_mount_config_t mount_cfg = {
        .format_if_mount_failed = config->format_if_mount_failed,
        .max_files              = config->max_files > 0 ? config->max_files : 5,
        .allocation_unit_size   = 0,
    };

    err = esp_vfs_fat_sdspi_mount(mount_point, &host, &dev_cfg, &mount_cfg, &s_card);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "esp_vfs_fat_sdspi_mount: %s", esp_err_to_name(err));
        spi_bus_free(PAPER_S3_SD_SPI_HOST);
        s_card = NULL;
        return err;
    }

    strlcpy(s_mount_point, mount_point, sizeof(s_mount_point));
    return ESP_OK;
}

esp_err_t bsp_sd_unmount(void) {
    if (!s_card) return ESP_ERR_INVALID_STATE;

    esp_err_t err = esp_vfs_fat_sdcard_unmount(s_mount_point, s_card);
    spi_bus_free(PAPER_S3_SD_SPI_HOST);
    s_card = NULL;
    return err;
}

bool bsp_sd_is_mounted(void) {
    return s_card != NULL;
}
