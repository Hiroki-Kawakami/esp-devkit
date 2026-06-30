cmake_minimum_required(VERSION 3.16.0)
# Pull the shared app component and the esp-devkit shared components.
# Paths are relative to this CMakeLists.txt (firmware/tab5_hello/esp32p4/).
set(EXTRA_COMPONENT_DIRS
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../bsp
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../ui_framework
    ${CMAKE_CURRENT_SOURCE_DIR}/../app)
set(BSP_BOARD tab5 CACHE STRING "esp-devkit BSP board")
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(tab5_hello)
