/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include "imgf_err.h"

const char *imgf_err_to_str(imgf_err_t err) {
    switch (err) {
        case IMGF_OK:                 return "Ok";
        case IMGF_ERR_TRUNCATED:      return "Truncated";
        case IMGF_ERR_DECODE:         return "DecodeError";
        case IMGF_ERR_UNSUPPORTED:    return "UnsupportedFormat";
        case IMGF_ERR_OOM:            return "OutOfMemory";
        case IMGF_ERR_INVALID_STATE:  return "InvalidState";
        case IMGF_ERR_INVALID_ARG:    return "InvalidArgument";
        case IMGF_ERR_TOO_LARGE:      return "TooLarge";
        default:                      return "Unknown";
    }
}
