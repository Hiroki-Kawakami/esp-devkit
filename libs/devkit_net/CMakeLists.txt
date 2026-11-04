# Network protocol stack.
set(requires idf rust_rt)
if(ESP_PLATFORM)
    list(APPEND requires esp_wifi)
endif()
idf_component_register(SRCS "/dev/null" PRIV_REQUIRES ${requires})
devkit_rust_crate(ROOT "src/lib.rs" TESTS)
