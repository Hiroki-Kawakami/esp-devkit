# Shared rust_hello app component.
idf_component_register(
    SRCS "rust_hello.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES bsp esp_timer idf rust_rt)
devkit_rust_crate(ROOT "lib.rs" TESTS
    BINDGEN_HEADERS "bsp.h" "esp_timer.h" "freertos/FreeRTOS.h" "freertos/task.h" "rust_hello.h"
    EXPORT_HEADERS "rust_hello_export.h")
