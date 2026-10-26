#pragma once

#include <stddef.h>
#include <stdint.h>

/* Implemented in lib.rs. */
uint32_t rust_hello_weight(uint8_t byte);

uint32_t rust_hello_checksum(const uint8_t *data, size_t len);
