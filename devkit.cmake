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
#   devkit_simulator()
#
# project() stays literal in the wrapper: CMake pre-scans the top-level
# CMakeLists.txt for it, and a project() hidden inside a macro triggers an
# implicit project(Project) that locks in the wrong (host default) toolchain.
set(DEVKIT_ROOT ${CMAKE_CURRENT_LIST_DIR})

include("${DEVKIT_ROOT}/tools/resgen/resgen.cmake")
include("${DEVKIT_ROOT}/tools/protogen/protogen.cmake")
include("${DEVKIT_ROOT}/tools/rust/rust.cmake")

# Device-side components, in EXTRA_COMPONENT_DIRS order.
set(DEVKIT_DEVICE_COMPONENTS
    bsp
    libs/audio_framework
    libs/compression_framework
    libs/devkit
    libs/esp_hosted_enhanced
    libs/harness
    libs/image_framework
    libs/jpeg_decode_enhanced
    libs/lvgl
    libs/media_framework
    libs/rust_rt
    libs/sensors
    libs/ui_framework
    libs/usb_device
    libs/usb_host
    libs/wifi)

# Simulator components: idf_compat first (simulator-only shims), then bsp and
# the libs.
set(DEVKIT_SIMULATOR_COMPONENTS
    idf_compat
    bsp
    libs/audio_framework
    libs/compression_framework
    libs/devkit
    libs/harness
    libs/image_framework
    libs/jpeg_decode_enhanced
    libs/lvgl
    libs/media_framework
    libs/rust_rt
    libs/sensors
    libs/ui_framework
    libs/usb_device
    libs/usb_host
    libs/wifi)

# devkit_idf_init([COMPONENT_DIRS dir...])
#
# Registers shared components and COMPONENT_DIRS in EXTRA_COMPONENT_DIRS, then
# trims the build to main's dependency graph via COMPONENTS. The wrapper calls
# project(<name>) right after this.
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
    set(CMAKE_CXX_STANDARD 23)
    set(CMAKE_CXX_STANDARD_REQUIRED ON)
    find_program(CCACHE_PROGRAM ccache)
    if(CCACHE_PROGRAM)
        set(CMAKE_C_COMPILER_LAUNCHER "${CCACHE_PROGRAM}")
        set(CMAKE_CXX_COMPILER_LAUNCHER "${CCACHE_PROGRAM}")
    endif()
endmacro()

# devkit_simulator([DEFAULT_ROTATION deg]
#                  [SDKCONFIG file] [SAVEDEFCONFIG file]
#                  [SDKCONFIG_DEFAULTS file...]
#                  [MAIN_SRCS src...] [COMPONENT_DIRS dir...])
#
# Builds the `simulator` host executable: host deps, an
# idf_component_register() shim that folds components straight into the
# executable, then adds the COMPONENT_DIRS components, idf_compat and the devkit
# components they reach through REQUIRES/PRIV_REQUIRES as subdirectories.
# MAIN_SRCS defaults to main/main.cpp if present; otherwise main() runs
# app_main(), which a COMPONENT_DIRS component (e.g. main) defines.
macro(devkit_simulator)
    cmake_parse_arguments(DEVKIT_SIM ""
        "BOARD;DEFAULT_ROTATION;SDKCONFIG;SAVEDEFCONFIG"
        "MAIN_SRCS;COMPONENT_DIRS;SDKCONFIG_DEFAULTS" ${ARGN})

    if(DEVKIT_SIM_BOARD)
        message(FATAL_ERROR
            "devkit_simulator: BOARD is no longer supported; select the board "
            "with CONFIG_BSP_BOARD_* in sdkconfig or sdkconfig.defaults")
    endif()
    if(NOT DEVKIT_SIM_DEFAULT_ROTATION)
        set(DEVKIT_SIM_DEFAULT_ROTATION 0)
    endif()
    if(NOT DEVKIT_SIM_MAIN_SRCS)
        if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/main/main.cpp")
            set(DEVKIT_SIM_MAIN_SRCS main/main.cpp)
        else()
            set(DEVKIT_SIM_MAIN_SRCS "${DEVKIT_ROOT}/tools/simulator/app_main.c")
        endif()
    endif()
    if(NOT DEVKIT_SIM_SDKCONFIG)
        set(DEVKIT_SIM_SDKCONFIG "${CMAKE_CURRENT_SOURCE_DIR}/sdkconfig")
    elseif(NOT IS_ABSOLUTE "${DEVKIT_SIM_SDKCONFIG}")
        set(DEVKIT_SIM_SDKCONFIG
            "${CMAKE_CURRENT_SOURCE_DIR}/${DEVKIT_SIM_SDKCONFIG}")
    endif()
    if(NOT DEVKIT_SIM_SDKCONFIG_DEFAULTS)
        set(DEVKIT_SIM_SDKCONFIG_DEFAULTS
            "${CMAKE_CURRENT_SOURCE_DIR}/sdkconfig.defaults")
    else()
        set(_devkit_sim_defaults "")
        foreach(_devkit_default ${DEVKIT_SIM_SDKCONFIG_DEFAULTS})
            if(NOT IS_ABSOLUTE "${_devkit_default}")
                set(_devkit_default
                    "${CMAKE_CURRENT_SOURCE_DIR}/${_devkit_default}")
            endif()
            list(APPEND _devkit_sim_defaults "${_devkit_default}")
        endforeach()
        set(DEVKIT_SIM_SDKCONFIG_DEFAULTS ${_devkit_sim_defaults})
    endif()
    if(NOT DEVKIT_SIM_SAVEDEFCONFIG)
        set(DEVKIT_SIM_SAVEDEFCONFIG
            "${CMAKE_CURRENT_SOURCE_DIR}/sdkconfig.defaults")
    elseif(NOT IS_ABSOLUTE "${DEVKIT_SIM_SAVEDEFCONFIG}")
        set(DEVKIT_SIM_SAVEDEFCONFIG
            "${CMAKE_CURRENT_SOURCE_DIR}/${DEVKIT_SIM_SAVEDEFCONFIG}")
    endif()

    set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

    # ---------------------------------------------------------------------------
    # Host dependencies
    # ---------------------------------------------------------------------------
    find_package(SDL2 REQUIRED)
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
    # libm is otherwise only implied when g++ links, i.e. a C++ component is in.
    target_link_libraries(simulator SDL2::SDL2 Threads::Threads
        PkgConfig::JPEG ${CMAKE_DL_LIBS} m)

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

    # Same resolution as IDF minus version.txt / project(VERSION), so
    # -DPROJECT_VER=<ver> overrides it on both targets.
    if(DEFINED PROJECT_VER)
        set(_devkit_sim_project_ver "${PROJECT_VER}")
    else()
        set(_devkit_sim_project_ver "")
        find_package(Git QUIET)
        if(GIT_FOUND)
            execute_process(
                COMMAND "${GIT_EXECUTABLE}" describe --always --tags --dirty
                WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
                OUTPUT_VARIABLE _devkit_sim_project_ver
                OUTPUT_STRIP_TRAILING_WHITESPACE
                ERROR_QUIET)
        endif()
        if(NOT _devkit_sim_project_ver)
            set(_devkit_sim_project_ver 1)
        endif()
    endif()
    string(SUBSTRING "${_devkit_sim_project_ver}" 0 31 _devkit_sim_project_ver)
    message(STATUS "App \"${PROJECT_NAME}\" version: ${_devkit_sim_project_ver}")
    set_source_files_properties("${DEVKIT_ROOT}/idf_compat/src/esp_app_desc.c"
        PROPERTIES COMPILE_DEFINITIONS
        "PROJECT_VER=\"${_devkit_sim_project_ver}\";PROJECT_NAME=\"${PROJECT_NAME}\"")

    # ---------------------------------------------------------------------------
    # Components (idf_component_register shim)
    # ---------------------------------------------------------------------------
    # Defined in a separate file: a macro text-substitutes its formal
    # parameters (${ARGN}, ...) into anything textually inside it, which would
    # corrupt the shim functions' own ${ARGN}. include() is not subject to
    # that substitution.
    include("${DEVKIT_ROOT}/tools/simulator/shim.cmake")

    set(DEVKIT_SIM_COMPONENT_PATHS "")
    foreach(_devkit_comp ${DEVKIT_SIMULATOR_COMPONENTS})
        list(APPEND DEVKIT_SIM_COMPONENT_PATHS "${DEVKIT_ROOT}/${_devkit_comp}")
    endforeach()
    set(DEVKIT_SIM_ROOTS "${DEVKIT_ROOT}/idf_compat")
    foreach(_devkit_dir ${DEVKIT_SIM_COMPONENT_DIRS})
        if(NOT IS_ABSOLUTE "${_devkit_dir}")
            set(_devkit_dir "${CMAKE_CURRENT_SOURCE_DIR}/${_devkit_dir}")
        endif()
        if(EXISTS "${_devkit_dir}/CMakeLists.txt")
            list(APPEND DEVKIT_SIM_COMPONENT_PATHS "${_devkit_dir}")
            list(APPEND DEVKIT_SIM_ROOTS "${_devkit_dir}")
        else()
            file(GLOB _devkit_children LIST_DIRECTORIES true "${_devkit_dir}/*")
            list(SORT _devkit_children)
            foreach(_devkit_child ${_devkit_children})
                if(EXISTS "${_devkit_child}/CMakeLists.txt")
                    list(APPEND DEVKIT_SIM_COMPONENT_PATHS "${_devkit_child}")
                    list(APPEND DEVKIT_SIM_ROOTS "${_devkit_child}")
                endif()
            endforeach()
        endif()
    endforeach()

    include("${DEVKIT_ROOT}/tools/simulator/kconfig.cmake")
    devkit_simulator_kconfig(
        SDKCONFIG "${DEVKIT_SIM_SDKCONFIG}"
        SDKCONFIG_DEFAULTS ${DEVKIT_SIM_SDKCONFIG_DEFAULTS}
        SAVEDEFCONFIG "${DEVKIT_SIM_SAVEDEFCONFIG}"
        COMPONENT_PATHS ${DEVKIT_SIM_COMPONENT_PATHS})

    include("${DEVKIT_ROOT}/tools/simulator/components.cmake")
    devkit_simulator_components(DEVKIT_SIM_COMPONENT_PATHS
        ROOTS ${DEVKIT_SIM_ROOTS}
        PATHS ${DEVKIT_SIM_COMPONENT_PATHS})

    foreach(_devkit_comp ${DEVKIT_SIM_COMPONENT_PATHS})
        get_filename_component(_devkit_comp_name "${_devkit_comp}" NAME)
        add_subdirectory("${_devkit_comp}" "${CMAKE_BINARY_DIR}/components/${_devkit_comp_name}")
    endforeach()

    # Host test executables are linked by rustc; this archive gives them the
    # idf_compat API, compiled as for the simulator.
    if(TARGET rust_test)
        get_property(_devkit_compat_srcs GLOBAL PROPERTY DEVKIT_SIM_SRCS_idf_compat)
        add_library(devkit_sim_idf_compat STATIC EXCLUDE_FROM_ALL ${_devkit_compat_srcs})
        target_include_directories(devkit_sim_idf_compat PRIVATE
            $<TARGET_PROPERTY:simulator,INCLUDE_DIRECTORIES>)
        target_compile_definitions(devkit_sim_idf_compat PRIVATE
            $<TARGET_PROPERTY:simulator,COMPILE_DEFINITIONS>)
        target_compile_options(devkit_sim_idf_compat PRIVATE
            $<TARGET_PROPERTY:simulator,COMPILE_OPTIONS>)
        set(_devkit_compat_link ${JPEG_LINK_LIBRARIES} ${CMAKE_THREAD_LIBS_INIT})
        foreach(_devkit_lib ${CMAKE_DL_LIBS} m)
            list(APPEND _devkit_compat_link "-l${_devkit_lib}")
        endforeach()
        set_property(TARGET devkit_sim_idf_compat PROPERTY DEVKIT_RUST_LINK_ARGS ${_devkit_compat_link})
    endif()
endmacro()
