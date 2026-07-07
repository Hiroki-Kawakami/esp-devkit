cmake_minimum_required(VERSION 3.16.0)
# Pull the shared app component and the esp-devkit bsp component.
# Paths are relative to this CMakeLists.txt (firmware/atom_lite_hello/esp32/).
set(EXTRA_COMPONENT_DIRS
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../bsp
    ${CMAKE_CURRENT_SOURCE_DIR}/../app)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(atom_lite_hello)
