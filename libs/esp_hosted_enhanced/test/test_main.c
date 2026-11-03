/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdlib.h>
#include <string.h>

#include "pbcheck.h"

extern const ProtobufCMessageDescriptor rpc__descriptor;
extern const pb_msgdesc_t rpc__msgdesc;

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "--dump") == 0) return pbcheck_dump(&rpc__descriptor, 20000, argv[2]);
    return pbcheck_run(&rpc__descriptor, &rpc__msgdesc, 50000) ? EXIT_FAILURE : EXIT_SUCCESS;
}
