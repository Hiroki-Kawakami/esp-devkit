cmake_minimum_required(VERSION 3.16.0)
# Pull the shared app component and the esp-devkit shared components.
# Paths are relative to this CMakeLists.txt (firmware/epd_tweak/esp32s3/).
set(EXTRA_COMPONENT_DIRS
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../bsp
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../ui_framework
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../libs/image_framework
    ${CMAKE_CURRENT_SOURCE_DIR}/../app)
set(BSP_BOARD paper_s3 CACHE STRING "esp-devkit BSP board")
set(EPD_LL_TWEAK 1 CACHE BOOL "compile the epd_ll waveform-tuning backdoor")
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(epd_tweak)
