# esp-devkit bsp — shared board-support component.
#
# The model-agnostic glue (src/) is built for both targets; the per-model
# bring-up under boards/<model>/ has a device variant (<model>.c, using the
# real chip drivers in devices/) and a simulator variant (<model>_sim.cpp,
# using the reusable SDL backend in simulator/). The build selects the variant
# by target: ESP_PLATFORM is defined only under ESP-IDF (device); the
# simulator consumes this file through the idf_component_register shim in
# simulator/CMakeLists.txt, where ESP_PLATFORM is unset.
# Model-agnostic display/touch/audio dispatch + the shared audio_dsp chain —
# built for both targets.
set(BSP_COMMON_SRCS
    "src/bsp_display.c"
    "src/bsp_touch.c")

if(ESP_PLATFORM)
    idf_component_register(
        SRCS
            ${BSP_COMMON_SRCS}
            "boards/paper_s3/paper_s3.c"
            "devices/gt911/gt911.c"
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "inc_private" "devices"
        PRIV_REQUIRES driver nvs_flash bt fatfs sdmmc esp_driver_sdmmc)

    # Enable Link Time Optimization
    target_compile_options(${COMPONENT_LIB} PRIVATE -flto)
    target_link_libraries(${COMPONENT_LIB} PRIVATE -flto)
else()
    # Simulator: SDL backend + the simulator host bring-up. SDL2 is already linked to
    # the `simulator` executable (see simulator/CMakeLists.txt), so its headers
    # are on the global include path here.
    idf_component_register(
        SRCS
            ${BSP_COMMON_SRCS}
            "boards/paper_s3/paper_s3_sim.c"
            "simulator/sdl_panel.c"
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "inc_private" "simulator")
endif()
