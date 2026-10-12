/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Differential check of the pb.h runtime against libprotobuf-c.
 */

#pragma once
#include <protobuf-c/protobuf-c.h>

#include "pb.h"

// Random messages from `ref` must decode to equal values and re-encode to the
// same bytes; mutated input must not crash. Returns the number of failures.
int pbcheck_run(const ProtobufCMessageDescriptor *ref, const pb_msgdesc_t *desc, unsigned iterations);

// Equality of a protobuf-c message and a pb.h message, logging the first difference.
bool pbcheck_equal(const ProtobufCMessageDescriptor *ref, const void *expect, const pb_msgdesc_t *desc,
                   const void *actual);
