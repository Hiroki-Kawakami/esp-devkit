/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "executor_shim.h"

#ifdef ESP_PLATFORM
#include "esp_memory_utils.h"
#include "hal/cache_ll.h"
#include "esp_adc/adc_cali_scheme.h"
#endif

static portMUX_TYPE s_mux __attribute__((unused)) = portMUX_INITIALIZER_UNLOCKED;
static portMUX_TYPE s_sync_mux __attribute__((unused)) = portMUX_INITIALIZER_UNLOCKED;

void idf_task_notify(TaskHandle_t task, UBaseType_t index, uint32_t bits) {
#ifdef ESP_PLATFORM
    if (xPortInIsrContext()) {
        BaseType_t woken = pdFALSE;
        xTaskGenericNotifyFromISR(task, index, bits, eSetBits, NULL, &woken);
        portYIELD_FROM_ISR(woken);
        return;
    }
#endif
    xTaskGenericNotify(task, index, bits, eSetBits, NULL);
}

void idf_executor_critical_enter(void) {
    portENTER_CRITICAL_SAFE(&s_mux);
}

void idf_executor_critical_exit(void) {
    portEXIT_CRITICAL_SAFE(&s_mux);
}

void idf_sync_critical_enter(void) {
    portENTER_CRITICAL(&s_sync_mux);
}

void idf_sync_critical_exit(void) {
    portEXIT_CRITICAL(&s_sync_mux);
}

void *idf_cache_uncached(void *cached) {
#ifdef CACHE_LL_L2MEM_NON_CACHE_ADDR
    return (void *)CACHE_LL_L2MEM_NON_CACHE_ADDR(cached);
#else
    return cached;
#endif
}

bool idf_ptr_dma_capable(const void *p) {
#ifdef ESP_PLATFORM
    return esp_ptr_dma_capable(p);
#else
    (void)p;
    return true;
#endif
}

#ifdef ESP_PLATFORM
esp_err_t idf_adc_cali_create(adc_unit_t unit, adc_channel_t channel, adc_atten_t atten, adc_cali_handle_t *out) {
#if ADC_CALI_SCHEME_CURVE_FITTING_SUPPORTED
    const adc_cali_curve_fitting_config_t config = {
        .unit_id = unit, .chan = channel, .atten = atten, .bitwidth = ADC_BITWIDTH_DEFAULT,
    };
    return adc_cali_create_scheme_curve_fitting(&config, out);
#elif ADC_CALI_SCHEME_LINE_FITTING_SUPPORTED
    (void)channel;
    const adc_cali_line_fitting_config_t config = {
        .unit_id = unit, .atten = atten, .bitwidth = ADC_BITWIDTH_DEFAULT,
    };
    return adc_cali_create_scheme_line_fitting(&config, out);
#else
    (void)unit; (void)channel; (void)atten; (void)out;
    return ESP_ERR_NOT_SUPPORTED;
#endif
}

void idf_adc_cali_delete(adc_cali_handle_t handle) {
#if ADC_CALI_SCHEME_CURVE_FITTING_SUPPORTED
    adc_cali_delete_scheme_curve_fitting(handle);
#elif ADC_CALI_SCHEME_LINE_FITTING_SUPPORTED
    adc_cali_delete_scheme_line_fitting(handle);
#else
    (void)handle;
#endif
}
#endif
