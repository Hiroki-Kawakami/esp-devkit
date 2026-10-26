#pragma once

#include <stddef.h>
#include <stdint.h>

uint32_t rust_hello_checksum(const uint8_t *data, size_t len);
uint32_t rust_hello_ms_to_ticks(uint32_t ms);
