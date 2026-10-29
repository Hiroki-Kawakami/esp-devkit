# Rust board support package.
if(ESP_PLATFORM)
    set(target_requires esp_psram)
else()
    set(target_requires sim_device)
endif()
idf_component_register(
    SRCS "/dev/null"
    REQUIRES idf
    PRIV_REQUIRES devkit rust_rt ${target_requires})
devkit_rust_crate(ROOT "src/lib.rs" TESTS)
