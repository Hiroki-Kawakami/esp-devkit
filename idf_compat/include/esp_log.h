#pragma once
// Host counterpart of ESP-IDF <esp_log.h>. Logs to stderr in roughly the IDF
// format ("<L> (<ms>) <tag>: <msg>"). Level filtering is not implemented:
// E/W/I always print, D/V are compiled out (esp_log_level_get() reports INFO for
// every tag), and esp_log_level_set() is a no-op kept only for source
// compatibility.
#include <stdint.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    ESP_LOG_NONE,
    ESP_LOG_ERROR,
    ESP_LOG_WARN,
    ESP_LOG_INFO,
    ESP_LOG_DEBUG,
    ESP_LOG_VERBOSE,
} esp_log_level_t;

uint32_t esp_log_timestamp(void);
esp_log_level_t esp_log_level_get(const char *tag);
void esp_log_write(esp_log_level_t level, const char *tag, const char *format, ...)
    __attribute__((format(printf, 3, 4)));

#define esp_log_level_set(tag, level) ((void)(tag), (void)(level))

#define ESP_LOG_LEVEL(letter, tag, fmt, ...) \
    fprintf(stderr, letter " (%u) %s: " fmt "\n", esp_log_timestamp(), tag, ##__VA_ARGS__)

#define ESP_LOGE(tag, fmt, ...) ESP_LOG_LEVEL("E", tag, fmt, ##__VA_ARGS__)
#define ESP_LOGW(tag, fmt, ...) ESP_LOG_LEVEL("W", tag, fmt, ##__VA_ARGS__)
#define ESP_LOGI(tag, fmt, ...) ESP_LOG_LEVEL("I", tag, fmt, ##__VA_ARGS__)
#define ESP_LOGD(tag, fmt, ...) ((void)(tag))
#define ESP_LOGV(tag, fmt, ...) ((void)(tag))

#ifdef __cplusplus
}
#endif
