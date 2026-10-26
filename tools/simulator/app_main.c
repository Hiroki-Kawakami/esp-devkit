/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Simulator entry for firmware without MAIN_SRCS: runs app_main() like IDF's
 * startup does, on the host's main thread.
 */

#include <stdio.h>

void app_main(void);

int main(void) {
    /* Line-buffered like the device console, so piped output is not lost. */
    setvbuf(stdout, NULL, _IOLBF, 0);
    app_main();
    return 0;
}
