# Harness line protocol in Rust (docs/harness.md).
if(ESP_PLATFORM)
    set(console_requires esp_driver_uart esp_driver_usb_serial_jtag vfs)
endif()
idf_component_register(
    SRCS "src/console.c"
    PRIV_INCLUDE_DIRS "src"
    REQUIRES bsp_rs idf
    PRIV_REQUIRES devkit_image rust_rt ${console_requires})
devkit_rust_crate(ROOT "src/lib.rs"
    BINDGEN_HEADERS "console.h")
