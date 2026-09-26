/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "recovery.h"

#include "driver/gpio.h"
#include "esp_flash.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_partition.h"
#include "esp_rom_sys.h"
#include "esp_system.h"

#define RECOVERY_GPIO   GPIO_NUM_2
#define SECTOR          4096
#define BOOTS_TO_SWITCH 3

static const char *TAG = "recovery";

/* 0 when a partition covers the last sector. */
static uint32_t counter_addr(void) {
    uint32_t size;
    if (esp_flash_get_size(NULL, &size) != ESP_OK) return 0;
    uint32_t addr = size - SECTOR;
    esp_partition_iterator_t it = esp_partition_find(ESP_PARTITION_TYPE_ANY, ESP_PARTITION_SUBTYPE_ANY, NULL);
    for (; it; it = esp_partition_next(it)) {
        const esp_partition_t *p = esp_partition_get(it);
        if (addr < p->address + p->size && p->address < addr + SECTOR) {
            esp_partition_iterator_release(it);
            return 0;
        }
    }
    return addr;
}

/* -1: the sector holds something other than counter marks. */
static int read_boots(uint32_t addr) {
    uint8_t marks[BOOTS_TO_SWITCH];
    if (esp_flash_read(NULL, marks, addr, sizeof(marks)) != ESP_OK) return -1;
    int n = 0;
    while (n < BOOTS_TO_SWITCH && marks[n] == 0x00) n++;
    for (int i = n; i < BOOTS_TO_SWITCH; i++) {
        if (marks[i] != 0xff) return -1;
    }
    return n;
}

void recovery_clear(void) {
    uint32_t addr = counter_addr();
    if (addr && read_boots(addr) != 0) esp_flash_erase_region(NULL, addr, SECTOR);
}

bool recovery_check(void) {
    gpio_config_t io = {
        .pin_bit_mask = BIT64(RECOVERY_GPIO),
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE,
    };
    gpio_config(&io);
    esp_rom_delay_us(100);
    bool requested = gpio_get_level(RECOVERY_GPIO) == 0;
    gpio_reset_pin(RECOVERY_GPIO);

    if (!requested) {
        recovery_clear();
        return false;
    }

    uint32_t addr = counter_addr();
    if (!addr) {
        ESP_LOGW(TAG, "safe mode, no free sector for the boot counter");
        return true;
    }
    int boots = read_boots(addr);
    if (boots < 0) {
        esp_flash_erase_region(NULL, addr, SECTOR);
        boots = 0;
    }
    const uint8_t mark = 0x00;
    esp_flash_write(NULL, &mark, addr + boots, 1);
    boots++;
    ESP_LOGW(TAG, "safe mode, boot %d of %d", boots, BOOTS_TO_SWITCH);

    if (boots >= BOOTS_TO_SWITCH) {
        esp_flash_erase_region(NULL, addr, SECTOR);
        const esp_partition_t *other = esp_ota_get_next_update_partition(NULL);
        if (other && esp_ota_set_boot_partition(other) == ESP_OK) {
            ESP_LOGW(TAG, "switching to %s", other->label);
            esp_restart();
        }
        ESP_LOGE(TAG, "no bootable slot to switch to");
    }
    return true;
}
