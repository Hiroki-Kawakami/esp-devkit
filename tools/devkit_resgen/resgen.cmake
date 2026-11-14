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
    set(${R_OUTPUTS} ${outputs} PARENT_SCOPE)
endfunction()
