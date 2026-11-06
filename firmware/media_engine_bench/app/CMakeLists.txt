# Checks media_engine against the IDF drivers on the same pictures.
set(requires devkit devkit_gfx idf media_engine rust_rt)
if(ESP_PLATFORM)
    list(APPEND requires esp_driver_ppa freertos)
endif()
idf_component_register(SRCS "/dev/null" PRIV_REQUIRES ${requires})
devkit_rust_crate(ROOT "lib.rs" BINDGEN_HEADERS "freertos/FreeRTOS.h" "freertos/queue.h" "freertos/semphr.h" "driver/ppa.h")
