/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Simulator entry for firmware without MAIN_SRCS: runs app_main() on a "main"
 * task like IDF's startup does.
 */

#include <stdio.h>
#include <unistd.h>

#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#define MAIN_TASK_STACK 16384
#define MAIN_TASK_PRIORITY 1

void app_main(void);

static void main_task(void *arg) {
    (void)arg;
    app_main();
    vTaskDelete(NULL);
}

int main(void) {
    /* Line-buffered like the device console, so piped output is not lost. */
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (xTaskCreatePinnedToCore(main_task, "main", MAIN_TASK_STACK, NULL, MAIN_TASK_PRIORITY, NULL, 0) != pdPASS) {
        fprintf(stderr, "no main task\n");
        return 1;
    }
    /* As on the device, the firmware keeps running after app_main returns. */
    for (;;) {
        pause();
    }
}
