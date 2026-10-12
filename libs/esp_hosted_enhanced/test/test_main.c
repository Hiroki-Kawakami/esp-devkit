/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdlib.h>

#include "pbcheck.h"

extern const ProtobufCMessageDescriptor rpc__descriptor;
extern const pb_msgdesc_t rpc__msgdesc;

int main(void) {
    return pbcheck_run(&rpc__descriptor, &rpc__msgdesc, 50000) ? EXIT_FAILURE : EXIT_SUCCESS;
}
