/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#ifdef __cplusplus
extern "C" {
#endif

typedef int imgf_err_t;

#define IMGF_OK                  0
#define IMGF_ERR_TRUNCATED       1
#define IMGF_ERR_DECODE          2
#define IMGF_ERR_UNSUPPORTED     3
#define IMGF_ERR_OOM             4
#define IMGF_ERR_INVALID_STATE   5
#define IMGF_ERR_INVALID_ARG     6
#define IMGF_ERR_TOO_LARGE       7

const char *imgf_err_to_str(imgf_err_t err);

#ifdef __cplusplus
}
#endif
