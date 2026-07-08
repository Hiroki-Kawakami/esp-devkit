# esp-devkit shared CMake glue.
#
# Device wrapper:
#   cmake_minimum_required(VERSION 3.16)
#   include(<esp-devkit>/devkit.cmake)
#   devkit_idf_init(COMPONENT_DIRS ../app)
#   project(my_app)
#
# Simulator wrapper:
#   cmake_minimum_required(VERSION 3.16)
#   include(<esp-devkit>/devkit.cmake)
#   devkit_simulator_init()
#   project(simulator C CXX)
#   devkit_simulator(BOARD airq)
#
# project() stays literal in the wrapper: CMake pre-scans the top-level
# CMakeLists.txt for it, and a project() hidden inside a macro triggers an
# implicit project(Project) that locks in the wrong (host default) toolchain.
set(DEVKIT_ROOT ${CMAKE_CURRENT_LIST_DIR})

# Device-side components, in EXTRA_COMPONENT_DIRS order.
set(DEVKIT_DEVICE_COMPONENTS
    bsp
    ui_framework
    libs/image_framework
    libs/jpeg_decode_enhanced)

# Simulator components: idf_compat/sim_harness first (simulator-only shims),
# then libs, then bsp/ui_framework.
set(DEVKIT_SIMULATOR_COMPONENTS
    idf_compat
    sim_harness
    bsp
    ui_framework
    libs/image_framework
    libs/jpeg_decode_enhanced)

# devkit_idf_init([COMPONENT_DIRS dir...])
#
# Registers all devkit components plus COMPONENT_DIRS in EXTRA_COMPONENT_DIRS,
# then trims the build to main's dependency graph via COMPONENTS: every devkit
# component is discoverable, but only the ones main (transitively) REQUIRES
# get built. The wrapper calls project(<name>) right after this.
macro(devkit_idf_init)
    cmake_parse_arguments(DEVKIT "" "" "COMPONENT_DIRS" ${ARGN})

    set(EXTRA_COMPONENT_DIRS "")
    foreach(_devkit_comp ${DEVKIT_DEVICE_COMPONENTS})
        list(APPEND EXTRA_COMPONENT_DIRS "${DEVKIT_ROOT}/${_devkit_comp}")
    endforeach()
    foreach(_devkit_dir ${DEVKIT_COMPONENT_DIRS})
        if(NOT IS_ABSOLUTE "${_devkit_dir}")
            set(_devkit_dir "${CMAKE_CURRENT_SOURCE_DIR}/${_devkit_dir}")
        endif()
        list(APPEND EXTRA_COMPONENT_DIRS "${_devkit_dir}")
    endforeach()

    set(COMPONENTS main)

    include($ENV{IDF_PATH}/tools/cmake/project.cmake)
endmacro()

# devkit_simulator_init()
#
# Everything that must precede project(): compiler selection, language
# standards, ccache. The wrapper calls project(simulator C CXX) right after.
macro(devkit_simulator_init)
    set(CMAKE_C_COMPILER gcc)
    set(CMAKE_CXX_COMPILER g++)
    set(CMAKE_C_STANDARD 11)
    set(CMAKE_CXX_STANDARD 17)
    find_program(CCACHE_PROGRAM ccache)
    if(CCACHE_PROGRAM)
        set(CMAKE_C_COMPILER_LAUNCHER "${CCACHE_PROGRAM}")
        set(CMAKE_CXX_COMPILER_LAUNCHER "${CCACHE_PROGRAM}")
    endif()
    set(FETCHCONTENT_UPDATES_DISCONNECTED ON)
endmacro()

# devkit_simulator(BOARD board
#                  [LV_CONF_DIR dir] [DEFAULT_ROTATION deg]
#                  [MAIN_SRCS src...] [COMPONENT_DIRS dir...])
#
# Builds the `simulator` host executable: host deps, an
# idf_component_register() shim that folds devkit components straight into the
# executable, then adds devkit components + COMPONENT_DIRS as subdirectories.
macro(devkit_simulator)
    cmake_parse_arguments(DEVKIT_SIM "" "BOARD;LV_CONF_DIR;DEFAULT_ROTATION"
        "MAIN_SRCS;COMPONENT_DIRS" ${ARGN})

    if(NOT DEVKIT_SIM_BOARD)
        message(FATAL_ERROR "devkit_simulator: BOARD is required")
    endif()
    if(NOT DEVKIT_SIM_DEFAULT_ROTATION)
        set(DEVKIT_SIM_DEFAULT_ROTATION 0)
    endif()
    if(NOT DEVKIT_SIM_MAIN_SRCS)
        set(DEVKIT_SIM_MAIN_SRCS main/main.cpp)
    endif()

    include(FetchContent)

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
    target_link_libraries(simulator SDL2::SDL2 cjson Threads::Threads
        PkgConfig::JPEG ${CMAKE_DL_LIBS})

    # Initial host-view rotation of the SDL window (r/l keys change it at runtime).
    set(SIM_DEFAULT_ROTATION ${DEVKIT_SIM_DEFAULT_ROTATION} CACHE STRING
        "Initial SDL simulator window rotation (0/90/180/270)")
    target_compile_definitions(simulator PRIVATE SDL_PANEL_DEFAULT_ROTATION=${SIM_DEFAULT_ROTATION})

    foreach(_devkit_src ${DEVKIT_SIM_MAIN_SRCS})
        if(NOT IS_ABSOLUTE "${_devkit_src}")
            set(_devkit_src "${CMAKE_CURRENT_SOURCE_DIR}/${_devkit_src}")
        endif()
        target_sources(simulator PRIVATE "${_devkit_src}")
    endforeach()
    target_include_directories(simulator PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/main")

    if(DEVKIT_SIM_LV_CONF_DIR)
        if(NOT IS_ABSOLUTE "${DEVKIT_SIM_LV_CONF_DIR}")
            set(DEVKIT_SIM_LV_CONF_DIR "${CMAKE_CURRENT_SOURCE_DIR}/${DEVKIT_SIM_LV_CONF_DIR}")
        endif()
        set(UI_FRAMEWORK_LV_CONF_DIR "${DEVKIT_SIM_LV_CONF_DIR}")
    endif()

    # ---------------------------------------------------------------------------
    # Components (idf_component_register shim)
    # ---------------------------------------------------------------------------
    # Defined in a separate file: a macro text-substitutes its formal
    # parameters (${ARGN}, ...) into anything textually inside it, which would
    # corrupt the shim functions' own ${ARGN}. include() is not subject to
    # that substitution.
    include("${DEVKIT_ROOT}/cmake/sim_shim.cmake")

    set(BSP_BOARD ${DEVKIT_SIM_BOARD} CACHE STRING "esp-devkit BSP board")

    set(DEVKIT_SIM_COMPONENT_PATHS "")
    foreach(_devkit_comp ${DEVKIT_SIMULATOR_COMPONENTS})
        list(APPEND DEVKIT_SIM_COMPONENT_PATHS "${DEVKIT_ROOT}/${_devkit_comp}")
    endforeach()
    foreach(_devkit_dir ${DEVKIT_SIM_COMPONENT_DIRS})
        if(NOT IS_ABSOLUTE "${_devkit_dir}")
            set(_devkit_dir "${CMAKE_CURRENT_SOURCE_DIR}/${_devkit_dir}")
        endif()
        list(APPEND DEVKIT_SIM_COMPONENT_PATHS "${_devkit_dir}")
    endforeach()
    foreach(_devkit_comp ${DEVKIT_SIM_COMPONENT_PATHS})
        get_filename_component(_devkit_comp_name "${_devkit_comp}" NAME)
        add_subdirectory("${_devkit_comp}" "${CMAKE_BINARY_DIR}/components/${_devkit_comp_name}")
    endforeach()
endmacro()
