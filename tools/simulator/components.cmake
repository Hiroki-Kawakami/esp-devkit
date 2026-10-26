# devkit_simulator_components(<out> ROOTS dir... PATHS dir...)
#
# Sets <out> to the PATHS components reachable from ROOTS through REQUIRES and
# PRIV_REQUIRES, in PATHS order. Requirements without a simulator component
# (IDF's driver, nvs_flash, ...) are ignored.
function(devkit_simulator_components out)
    cmake_parse_arguments(C "" "" "ROOTS;PATHS" ${ARGN})

    set(scan "${CMAKE_BINARY_DIR}/config/requirements.txt")
    execute_process(
        COMMAND "${CMAKE_COMMAND}"
                "-DCOMPONENT_PATHS=${C_PATHS}"
                "-DSDKCONFIG_CMAKE=${CMAKE_BINARY_DIR}/config/sdkconfig.cmake"
                "-DOUT=${scan}"
                -P "${DEVKIT_ROOT}/tools/simulator/requirements.cmake"
        RESULT_VARIABLE result)
    if(NOT result EQUAL 0)
        message(FATAL_ERROR "devkit_simulator: reading component requirements failed")
    endif()
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS
        "${DEVKIT_ROOT}/tools/simulator/requirements.cmake")

    file(STRINGS "${scan}" lines)
    set(names "")
    foreach(line ${lines})
        separate_arguments(entry UNIX_COMMAND "${line}")
        list(POP_FRONT entry name)
        list(APPEND names "${name}")
        set(_reqs_${name} ${entry})
    endforeach()

    set(pending "")
    foreach(root ${C_ROOTS})
        get_filename_component(name "${root}" NAME)
        list(APPEND pending "${name}")
    endforeach()
    set(reached "")
    while(pending)
        list(POP_FRONT pending name)
        if(name IN_LIST reached OR NOT name IN_LIST names)
            continue()
        endif()
        list(APPEND reached "${name}")
        list(APPEND pending ${_reqs_${name}})
    endwhile()

    set(paths "")
    set(used "")
    foreach(dir ${C_PATHS})
        get_filename_component(name "${dir}" NAME)
        if(name IN_LIST reached)
            list(APPEND paths "${dir}")
            list(APPEND used "${name}")
        endif()
    endforeach()
    list(JOIN used " " used)
    message(STATUS "Simulator components: ${used}")
    set(${out} ${paths} PARENT_SCOPE)
endfunction()
