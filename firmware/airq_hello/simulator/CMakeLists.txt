cmake_minimum_required(VERSION 3.16)
include(FetchContent)

set(CMAKE_C_COMPILER gcc)
set(CMAKE_CXX_COMPILER g++)
set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)
set(FETCHCONTENT_UPDATES_DISCONNECTED ON)

project(simulator C CXX)

set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

# ---------------------------------------------------------------------------
# Host dependencies
# ---------------------------------------------------------------------------
find_package(SDL2 REQUIRED)
find_package(CJSON REQUIRED)     # idf_compat's nvs shim persists to a JSON blob
find_package(Threads REQUIRED)   # host FreeRTOS API (idf_compat) is built on pthreads
find_package(PkgConfig REQUIRED)
pkg_check_modules(JPEG REQUIRED IMPORTED_TARGET libjpeg)

# ---------------------------------------------------------------------------
# Executable
# ---------------------------------------------------------------------------
add_executable(simulator)
target_compile_definitions(simulator PRIVATE
    SDL_DISABLE_ARM_NEON_H
    SDL_MAIN_HANDLED)
target_link_libraries(simulator SDL2::SDL2 cjson Threads::Threads PkgConfig::JPEG ${CMAKE_DL_LIBS})

# Initial host-view rotation of the SDL window (r/l keys change it at runtime).
set(SIM_DEFAULT_ROTATION 0 CACHE STRING "Initial SDL simulator window rotation (0/90/180/270)")
target_compile_definitions(simulator PRIVATE SDL_PANEL_DEFAULT_ROTATION=${SIM_DEFAULT_ROTATION})

target_sources(simulator PRIVATE main/main.cpp)
target_include_directories(simulator PRIVATE main)

# ---------------------------------------------------------------------------
# Components (idf_component_register shim)
# ---------------------------------------------------------------------------
# Both the simulator-only layer (idf_compat) and the shared components describe
# themselves with the ESP-IDF idf_component_register() call in their own
# CMakeLists.txt. On device that is the real IDF function; here we shim it to
# fold the component's sources / include dirs straight into the `simulator`
# executable.
function(idf_component_register)
    cmake_parse_arguments(C "" ""
        "SRCS;INCLUDE_DIRS;PRIV_INCLUDE_DIRS;REQUIRES;PRIV_REQUIRES;EMBED_FILES;EMBED_TXTFILES"
        ${ARGN})
    foreach(src ${C_SRCS})
        if(NOT IS_ABSOLUTE "${src}")
            set(src "${CMAKE_CURRENT_SOURCE_DIR}/${src}")
        endif()
        target_sources(simulator PRIVATE "${src}")
    endforeach()
    foreach(dir ${C_INCLUDE_DIRS} ${C_PRIV_INCLUDE_DIRS})
        if(NOT IS_ABSOLUTE "${dir}")
            set(dir "${CMAKE_CURRENT_SOURCE_DIR}/${dir}")
        endif()
        target_include_directories(simulator PRIVATE "${dir}")
    endforeach()
endfunction()

function(idf_component_set_property)
endfunction()

# Select the M5 Air Quality Kit board for the shared bsp component.
set(BSP_BOARD airq CACHE STRING "esp-devkit BSP board")

set(SIMULATOR_COMPONENTS
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../idf_compat
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../sim_harness
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../bsp
    ${CMAKE_CURRENT_SOURCE_DIR}/../../../ui_framework
    ${CMAKE_CURRENT_SOURCE_DIR}/../app)
foreach(comp ${SIMULATOR_COMPONENTS})
    get_filename_component(name "${comp}" NAME)
    add_subdirectory("${comp}" "${CMAKE_BINARY_DIR}/components/${name}")
endforeach()
