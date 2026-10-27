#pragma once
// Host counterpart of ESP-IDF <esp_system.h>. esp_restart() exits the
// simulator with status 0.

#ifdef __cplusplus
extern "C" {
#endif

void esp_restart(void) __attribute__((__noreturn__));

#ifdef __cplusplus
}
#endif
