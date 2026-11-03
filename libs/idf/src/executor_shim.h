/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * FreeRTOS macros behind idf::executor, idf::sync and idf::thread.
 */

#pragma once

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
