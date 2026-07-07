cmake_minimum_required(VERSION 3.16.0)
# Pull the shared app component and the esp-devkit shared components.
# Paths are relative to this CMakeLists.txt (firmware/s31_korvo_hello/esp32s31/).
set(EXTRA_COMPONENT_DIRS
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../bsp
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../ui_framework
    ${CMAKE_CURRENT_SOURCE_DIR}/../app)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(s31_korvo_hello)
