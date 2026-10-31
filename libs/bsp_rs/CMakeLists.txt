# Rust board support package.
if(ESP_PLATFORM)
    set(target_requires esp_psram)
else()
    set(sim_headers "simulator/sim_psram.h" "stdlib.h" "unistd.h" "sys/socket.h")
endif()
idf_component_register(
    SRCS "/dev/null"
    REQUIRES devkit_audio idf
    PRIV_REQUIRES devkit rust_rt ${target_requires})
devkit_rust_crate(ROOT "src/lib.rs" TESTS
    BINDGEN_HEADERS ${sim_headers})
