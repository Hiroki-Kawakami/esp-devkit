#include "esp_log.h"

#include <stdarg.h>
#include <time.h>

uint32_t esp_log_timestamp(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint32_t)((uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u);
}

esp_log_level_t esp_log_level_get(const char *tag) {
    (void)tag;
    return ESP_LOG_INFO;
}

void esp_log_write(esp_log_level_t level, const char *tag, const char *format, ...) {
    if (level > esp_log_level_get(tag)) return;
    va_list args;
    va_start(args, format);
    vfprintf(stderr, format, args);
    va_end(args);
}
