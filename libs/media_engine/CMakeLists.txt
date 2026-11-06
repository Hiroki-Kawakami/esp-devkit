# The chip's media accelerators: 2D engine and JPEG codec.
set(srcs "/dev/null")
set(headers "")
set(options "")
set(priv_requires devkit rust_rt)
if(ESP_PLATFORM AND IDF_TARGET STREQUAL "esp32p4")
    set(srcs "src/clock.c")
    set(headers "hal/jpeg_ll.h" "hal/jpeg_hal.h" "hal/ppa_ll.h" "hal/dma2d_ll.h" "soc/dma2d_channel.h"
                "soc/ppa_struct.h" "hal/dma2d_types.h" "hal/ppa_types.h" "esp_private/dma2d.h" "esp_intr_alloc.h" "clock.h")
    # Shadowed by macros that only work inside PERIPH_RCC_ATOMIC(); clock.c calls them there.
    set(options "--blocklist-function=(jpeg_ll_enable_bus_clock|jpeg_ll_reset_module_register|ppa_ll_enable_bus_clock|ppa_ll_reset_register)")
    list(APPEND priv_requires esp_hal_jpeg esp_hal_ppa esp_hal_dma esp_driver_dma esp_hw_support esp_mm hal)
endif()
idf_component_register(SRCS ${srcs} PRIV_INCLUDE_DIRS "src" REQUIRES devkit_gfx idf PRIV_REQUIRES ${priv_requires})
devkit_rust_crate(ROOT "src/lib.rs" TESTS BINDGEN_HEADERS ${headers} BINDGEN_OPTIONS ${options})
