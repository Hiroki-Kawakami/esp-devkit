#pragma once
// Host counterpart of ESP-IDF <esp_random.h>, from the OS entropy source.
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

uint32_t esp_random(void);

#ifdef __cplusplus
}
#endif
