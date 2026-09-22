/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * image_framework decode / resize benchmark. Same source on the ESP32-P4 and
 * on the host, so a change can be measured on both.
 */

#pragma once

#ifdef __cplusplus
extern "C" {
#endif

void imgf_bench_run(void);

#ifdef __cplusplus
}
#endif
