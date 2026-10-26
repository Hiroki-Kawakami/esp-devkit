/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdio.h>

#include "rust_hello_export.h"

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    app_entry();
    return 0;
}
