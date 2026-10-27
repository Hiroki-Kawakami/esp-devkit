/*
 * Host counterpart of ESP-IDF's <freertos/idf_additions.h>: the *WithCaps task
 * functions. The stack caps are ignored on the host.
 */
#pragma once

#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#ifdef __cplusplus
extern "C" {
#endif

BaseType_t xTaskCreatePinnedToCoreWithCaps(TaskFunction_t pvTaskCode, const char *pcName,
                                           const configSTACK_DEPTH_TYPE usStackDepth,
                                           void *pvParameters, UBaseType_t uxPriority,
                                           TaskHandle_t *pvCreatedTask, BaseType_t xCoreID,
                                           UBaseType_t uxMemoryCaps);
void vTaskDeleteWithCaps(TaskHandle_t xTaskToDelete);

#ifdef __cplusplus
}
#endif
