# Standalone Kconfig integration for the host simulator build.
#
# Shared components expose the same Kconfig file to ESP-IDF and the simulator.
# The simulator keeps its own sdkconfig and uses esp-idf-kconfig's kconfgen to
# generate the sdkconfig.h and sdkconfig.cmake consumed by component sources and
# CMakeLists.txt files.

function(devkit_simulator_kconfig)
    cmake_parse_arguments(KCONFIG "" "SDKCONFIG;SAVEDEFCONFIG"
        "COMPONENT_PATHS;SDKCONFIG_DEFAULTS" ${ARGN})

    if(NOT TARGET simulator)
        message(FATAL_ERROR
            "devkit_simulator_kconfig must be called after creating the simulator target")
    endif()
    if(NOT KCONFIG_SDKCONFIG)
        message(FATAL_ERROR "devkit_simulator_kconfig: SDKCONFIG is required")
    endif()
    if(NOT KCONFIG_SAVEDEFCONFIG)
        message(FATAL_ERROR "devkit_simulator_kconfig: SAVEDEFCONFIG is required")
    endif()

    find_package(Python3 COMPONENTS Interpreter REQUIRED)
    execute_process(
        COMMAND ${Python3_EXECUTABLE} -c "import kconfgen"
        RESULT_VARIABLE _devkit_kconfgen_result
        OUTPUT_QUIET
        ERROR_QUIET)
    if(NOT _devkit_kconfgen_result EQUAL 0)
        message(FATAL_ERROR
            "The Python package 'esp-idf-kconfig' is required for simulator builds. "
            "Run CMake from the esp-devkit development environment (nix develop).")
    endif()

    set(_devkit_kconfigs "")
    foreach(_devkit_component ${KCONFIG_COMPONENT_PATHS})
        file(GLOB _devkit_component_kconfigs
            LIST_DIRECTORIES false
            CONFIGURE_DEPENDS
            "${_devkit_component}/Kconfig")
        list(APPEND _devkit_kconfigs ${_devkit_component_kconfigs})
    endforeach()

    set(_devkit_defaults_files "")
    foreach(_devkit_defaults ${KCONFIG_SDKCONFIG_DEFAULTS})
        file(GLOB _devkit_existing_defaults
            LIST_DIRECTORIES false
            CONFIGURE_DEPENDS
            "${_devkit_defaults}")
        list(APPEND _devkit_defaults_files ${_devkit_existing_defaults})
    endforeach()

    set(_devkit_config_dir "${CMAKE_BINARY_DIR}/config")
    set(_devkit_kconfigs_file "${_devkit_config_dir}/kconfigs.in")
    set(_devkit_sdkconfig_header "${_devkit_config_dir}/sdkconfig.h")
    set(_devkit_sdkconfig_cmake "${_devkit_config_dir}/sdkconfig.cmake")
    set(_devkit_sdkconfig_json "${_devkit_config_dir}/sdkconfig.json")
    get_filename_component(_devkit_sdkconfig_dir "${KCONFIG_SDKCONFIG}" DIRECTORY)
    get_filename_component(_devkit_savedefconfig_dir "${KCONFIG_SAVEDEFCONFIG}" DIRECTORY)
    file(MAKE_DIRECTORY "${_devkit_config_dir}")
    file(MAKE_DIRECTORY "${_devkit_sdkconfig_dir}")
    file(MAKE_DIRECTORY "${_devkit_savedefconfig_dir}")

    set(_devkit_kconfigs_content "")
    foreach(_devkit_kconfig ${_devkit_kconfigs})
        string(REPLACE "\\" "\\\\" _devkit_kconfig_escaped "${_devkit_kconfig}")
        string(REPLACE "\"" "\\\"" _devkit_kconfig_escaped "${_devkit_kconfig_escaped}")
        string(APPEND _devkit_kconfigs_content
            "source \"${_devkit_kconfig_escaped}\"\n")
    endforeach()
    file(WRITE "${_devkit_kconfigs_file}" "${_devkit_kconfigs_content}")

    set(_devkit_kconfgen_command
        ${Python3_EXECUTABLE} -m kconfgen
        --kconfig "${DEVKIT_ROOT}/Kconfig.simulator"
        --config "${KCONFIG_SDKCONFIG}"
        --env "COMPONENT_KCONFIGS_SOURCE_FILE=${_devkit_kconfigs_file}")
    foreach(_devkit_defaults ${_devkit_defaults_files})
        list(APPEND _devkit_kconfgen_command --defaults "${_devkit_defaults}")
    endforeach()

    set(_devkit_kconfgen_outputs
        --output header "${_devkit_sdkconfig_header}"
        --output cmake "${_devkit_sdkconfig_cmake}"
        --output json "${_devkit_sdkconfig_json}")
    if(NOT EXISTS "${KCONFIG_SDKCONFIG}")
        list(APPEND _devkit_kconfgen_outputs
            --output config "${KCONFIG_SDKCONFIG}")
    endif()

    execute_process(
        COMMAND ${_devkit_kconfgen_command} ${_devkit_kconfgen_outputs}
        RESULT_VARIABLE _devkit_kconfgen_result
        OUTPUT_VARIABLE _devkit_kconfgen_stdout
        ERROR_VARIABLE _devkit_kconfgen_stderr)
    if(NOT _devkit_kconfgen_result EQUAL 0)
        message(FATAL_ERROR
            "Failed to generate the simulator Kconfig outputs:\n"
            "${_devkit_kconfgen_stdout}${_devkit_kconfgen_stderr}")
    endif()

    include("${_devkit_sdkconfig_cmake}")
    foreach(_devkit_config ${CONFIGS_LIST})
        set(${_devkit_config} "${${_devkit_config}}" PARENT_SCOPE)
    endforeach()
    set(CONFIGS_LIST "${CONFIGS_LIST}" PARENT_SCOPE)
    target_include_directories(simulator PRIVATE "${_devkit_config_dir}")

    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS
        "${DEVKIT_ROOT}/Kconfig.simulator"
        "${KCONFIG_SDKCONFIG}"
        "${DEVKIT_ROOT}/cmake/sim_kconfig.cmake")
    foreach(_devkit_input ${_devkit_kconfigs} ${_devkit_defaults_files})
        set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS
            "${_devkit_input}")
    endforeach()

    add_custom_target(menuconfig
        COMMAND ${_devkit_kconfgen_command}
            --menuconfig
            --output config "${KCONFIG_SDKCONFIG}"
        USES_TERMINAL
        VERBATIM)

    add_custom_target(save-defconfig
        COMMAND ${_devkit_kconfgen_command}
            --dont-write-deprecated
            --output savedefconfig "${KCONFIG_SAVEDEFCONFIG}"
        USES_TERMINAL
        VERBATIM)

    message(STATUS "Simulator sdkconfig file ${KCONFIG_SDKCONFIG}")
endfunction()
