/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * FreeRTOS macros behind idf::executor.
 */

#pragma once

#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

/* From a task or an interrupt. */
void idf_executor_notify(TaskHandle_t task);

/* One spinlock for every executor; nests, and works in interrupts. */
void idf_executor_critical_enter(void);
void idf_executor_critical_exit(void);
