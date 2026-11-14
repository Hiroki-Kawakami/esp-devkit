set(RESGEN_DIR "${CMAKE_CURRENT_LIST_DIR}")

# Generates resources.rs and its binaries in `OUT`, a crate's OUT_DIR, and
# returns the generated files in `OUTPUTS`.
function(resgen_rust_resources)
    cmake_parse_arguments(R "" "DEFINITION;OUT;OUTPUTS" "" ${ARGN})
    get_filename_component(definition "${R_DEFINITION}" ABSOLUTE)
    find_program(DEVKIT_RESGEN devkit-resgen REQUIRED)
    set(fonts "${RESGEN_DIR}/fonts")
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${definition}" "${DEVKIT_RESGEN}")
    execute_process(
        COMMAND "${DEVKIT_RESGEN}" inputs "${definition}" "${fonts}" "${R_OUT}/entries.cmake"
        RESULT_VARIABLE result)
    if(NOT result EQUAL 0)
        message(FATAL_ERROR "resgen: invalid definition ${definition}")
    endif()
    include("${R_OUT}/entries.cmake")
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS ${RESGEN_THEME_FILES})

    set(outputs "${R_OUT}/resources.rs")
    add_custom_command(
        OUTPUT "${R_OUT}/resources.rs"
        COMMAND "${DEVKIT_RESGEN}" index "${definition}" "${fonts}" "${R_OUT}"
        DEPENDS "${definition}" "${DEVKIT_RESGEN}" ${RESGEN_THEME_FILES}
        COMMENT "resgen: resources.rs"
        VERBATIM)
    foreach(entry ${RESGEN_ENTRIES})
        add_custom_command(
            OUTPUT "${R_OUT}/resgen/${entry}.bin"
            COMMAND "${DEVKIT_RESGEN}" entry "${definition}" "${fonts}" "${R_OUT}" "${entry}"
            DEPENDS "${definition}" "${DEVKIT_RESGEN}" ${RESGEN_INPUTS_${entry}}
            COMMENT "resgen: ${entry}"
            VERBATIM)
        list(APPEND outputs "${R_OUT}/resgen/${entry}.bin")
    endforeach()
    foreach(label ${RESGEN_PARTITIONS})
        set(size 0)
        if(ESP_PLATFORM)
            partition_table_get_partition_info(size "--partition-name ${label}" "size")
            if(NOT size)
                message(FATAL_ERROR "resgen: no partition '${label}' in the partition table")
            endif()
        endif()
        set(image "${R_OUT}/resgen/${label}.partition")
        set(entries "")
        foreach(entry ${RESGEN_PARTITION_ENTRIES_${label}})
            list(APPEND entries "${R_OUT}/resgen/${entry}.bin")
        endforeach()
        add_custom_command(
            OUTPUT "${image}" "${R_OUT}/resgen/${label}.rs"
            COMMAND "${DEVKIT_RESGEN}" partition "${definition}" "${fonts}" "${R_OUT}" "${label}" "${size}"
            DEPENDS "${definition}" "${DEVKIT_RESGEN}" ${entries}
            COMMENT "resgen: partition ${label}"
            VERBATIM)
        list(APPEND outputs "${image}" "${R_OUT}/resgen/${label}.rs")
        if(ESP_PLATFORM)
            add_custom_target(resgen_${label} DEPENDS "${image}")
            esp_partition_register_target(${label} "${image}" FLASH_IN_PROJECT DEPENDS resgen_${label})
        endif()
    endforeach()
    set(${R_OUTPUTS} ${outputs} PARENT_SCOPE)
endfunction()
