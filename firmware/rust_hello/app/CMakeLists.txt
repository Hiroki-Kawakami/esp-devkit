# Shared rust_hello app component.
idf_component_register(
    SRCS "rust_hello.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES bsp rust_rt)
devkit_rust_crate(ROOT "lib.rs")
