# jpeg_decode_enhanced — strip-pipelined HW JPEG decode + PPA (device) with a
# host (simulator) backend so the same public API compiles for both targets.
#
# Layer 2 (jpeg_ppa_pipeline.c) is hardware-agnostic and shared by both targets.
# Layer 1 is target-specific: the device build uses the 2D-DMA strip decoder
# (jpeg_decode_enhanced.c, reaches into esp_driver_jpeg private structs); the
# simulator uses a whole-frame CPU decode backed by image_framework
# (jpeg_decode_enhanced_host.c).
#
# ESP_PLATFORM is defined only under ESP-IDF (device); the simulator consumes
# this file through the idf_component_register shim in simulator/CMakeLists.txt.

if(ESP_PLATFORM)
    idf_component_register(
        SRCS
            "src/jpeg_decode_enhanced.c"
            "src/jpeg_ppa_pipeline.c"
        INCLUDE_DIRS "include"
        REQUIRES esp_driver_jpeg esp_driver_ppa esp_driver_dma esp_hal_jpeg
        PRIV_REQUIRES hal esp_mm esp_hw_support esp_pm driver)

    # jpeg_decode_enhanced replays the IDF decode flow and depends on
    # esp_driver_jpeg internal headers that are not exported as public include
    # dirs. jpeg_private.h lives at the component root; the marker parsers and
    # parameter tables in private/. (The HAL func tables dqt_func/sof_func/
    # dht_func moved to esp_hal_jpeg's public hal/jpeg_hal.h in IDF v6, so those
    # come in via REQUIRES.) jpeg_decode_enhanced.c carries an ESP_IDF_VERSION
    # guard: the private struct layouts can change between IDF releases.
    target_include_directories(${COMPONENT_LIB} PRIVATE
        "$ENV{IDF_PATH}/components/esp_driver_jpeg"
        "$ENV{IDF_PATH}/components/esp_driver_jpeg/private")
else()
    idf_component_register(
        SRCS
            "src/jpeg_decode_enhanced_host.c"
            "src/jpeg_ppa_pipeline.c"
        INCLUDE_DIRS "include"
        REQUIRES image_framework)
endif()
