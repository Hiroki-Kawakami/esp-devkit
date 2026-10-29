# Rust board support package.
idf_component_register(
    SRCS "/dev/null"
    REQUIRES idf
    PRIV_REQUIRES devkit rust_rt esp_psram)
devkit_rust_crate(ROOT "src/lib.rs")
