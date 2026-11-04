# Shared rust_hello app component.
set(requires devkit_net idf rust_rt)
if(NOT ESP_PLATFORM OR IDF_TARGET STREQUAL "esp32p4")
    list(APPEND requires bsp_rs harness_rs)
endif()
idf_component_register(
    SRCS "rust_hello.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES ${requires})
devkit_rust_crate(ROOT "lib.rs" TESTS
    BINDGEN_HEADERS "rust_hello.h"
    EXPORT_HEADERS "rust_hello_export.h")
