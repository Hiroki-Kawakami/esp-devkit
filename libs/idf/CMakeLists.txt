# Rust bindings and safe wrappers for ESP-IDF APIs.
idf_component_register(
    SRCS "/dev/null"
    REQUIRES esp_common esp_system esp_timer log
    PRIV_REQUIRES devkit rust_rt)
if(ESP_PLATFORM)
    set(log_headers "esp_log.h" "esp_log_config.h" "esp_log_level.h" "esp_log_timestamp.h"
                    "esp_log_write.h")
else()
    set(log_headers "esp_log.h")
endif()
devkit_rust_crate(ROOT "src/lib.rs" TESTS
    BINDGEN_HEADERS "esp_err.h" "esp_system.h" "esp_timer.h" ${log_headers})
