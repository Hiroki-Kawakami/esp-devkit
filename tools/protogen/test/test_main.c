/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <stdio.h>
#include <stdlib.h>

#include "pbcheck.h"

extern const ProtobufCMessageDescriptor test_all__descriptor;
extern const pb_msgdesc_t test_all__msgdesc;

static int check_unpacked_repeated(void) {
    // r_u32 = 1, 300 unpacked, then 5, 6 packed.
    static const uint8_t wire[] = {0x90, 0x01, 0x01, 0x90, 0x01, 0xAC, 0x02, 0x92, 0x01, 0x02, 0x05, 0x06};
    ProtobufCMessage *ref = protobuf_c_message_unpack(&test_all__descriptor, NULL, sizeof(wire), wire);
    void *dec = pb_unpack(&test_all__msgdesc, NULL, wire, sizeof(wire));
    bool ok = ref && dec && pbcheck_equal(&test_all__descriptor, ref, &test_all__msgdesc, dec);
    printf("unpacked repeated: %s\n", ok ? "ok" : "FAILED");
    pb_free(dec);
    protobuf_c_message_free_unpacked(ref, NULL);
    return !ok;
}

int main(void) {
    int failures = check_unpacked_repeated();
    failures += pbcheck_run(&test_all__descriptor, &test_all__msgdesc, 20000);
    return failures ? EXIT_FAILURE : EXIT_SUCCESS;
}
