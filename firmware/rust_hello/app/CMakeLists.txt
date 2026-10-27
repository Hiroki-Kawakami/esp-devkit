# Shared rust_hello app component.
idf_component_register(
    SRCS "rust_hello.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES bsp idf rust_rt)
devkit_rust_crate(ROOT "lib.rs" TESTS
    BINDGEN_HEADERS "bsp.h" "rust_hello.h"
    EXPORT_HEADERS "rust_hello_export.h")
