# Shared rust_hello app component.
idf_component_register(
    SRCS "rust_hello.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES bsp rust_rt)
devkit_rust_crate(ROOT "lib.rs"
    BINDGEN_HEADERS "bsp.h" "esp_system.h" "freertos/FreeRTOS.h" "freertos/task.h" "stdio.h"
                    "rust_hello.h"
    EXPORT_HEADERS "rust_hello_weight.h")
