/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Macros behind idf::adc, idf::cache, idf::executor, idf::gpio, idf::heap,
 * idf::sync and idf::thread.
 */

#pragma once

#include <stdbool.h>

#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

/* Sets `bits` in the notification value at `index`; from a task or an interrupt. */
void idf_task_notify(TaskHandle_t task, UBaseType_t index, uint32_t bits);

/* One spinlock for every executor; nests, and works in interrupts. */
void idf_executor_critical_enter(void);
void idf_executor_critical_exit(void);

/* One spinlock for every idf::sync lock. */
void idf_sync_critical_enter(void);
void idf_sync_critical_exit(void);

/* The non-cacheable alias of internal RAM at `cached`; `cached` itself where
   internal RAM is not cached. */
void *idf_cache_uncached(void *cached);

/* Whether DMA reads `p` where it is, without being told it is in PSRAM. */
bool idf_ptr_dma_capable(const void *p);

#ifdef ESP_PLATFORM
#include "esp_adc/adc_cali.h"

/* The chip's calibration scheme for an ADC channel; ESP_ERR_NOT_SUPPORTED
   when it has none or its eFuse values are missing. */
esp_err_t idf_adc_cali_create(adc_unit_t unit, adc_channel_t channel, adc_atten_t atten, adc_cali_handle_t *out);
void idf_adc_cali_delete(adc_cali_handle_t handle);

/* Drives an output pin straight through its register, without checks. */
void idf_gpio_set_level_fast(uint32_t pin, uint32_t level);
#endif
