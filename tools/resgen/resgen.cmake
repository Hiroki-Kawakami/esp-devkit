set(RESGEN_DIR "${CMAKE_CURRENT_LIST_DIR}")
set(RESGEN_SCRIPT "${RESGEN_DIR}/resgen.py")

function(resgen_add_resources)
    cmake_parse_arguments(R "" "TARGET;DEFINITION;PARTITION" "" ${ARGN})
    if(NOT R_TARGET OR NOT R_DEFINITION)
        message(FATAL_ERROR "resgen_add_resources: TARGET and DEFINITION are required")
    endif()
    get_filename_component(definition "${R_DEFINITION}" ABSOLUTE)

    set(python "$ENV{RESGEN_PYTHON}")
    if(NOT python)
        message(FATAL_ERROR "resgen: RESGEN_PYTHON is not set; run the build through `nix develop`")
    endif()

    set(blob_flag "")
    if(R_PARTITION)
        set(blob_flag --blob)
    endif()

    set(out "${CMAKE_CURRENT_BINARY_DIR}/resgen")
    set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${definition}" "${RESGEN_SCRIPT}")
    execute_process(
        COMMAND "${python}" "${RESGEN_SCRIPT}" cmake "${definition}" "${out}/entries.cmake"
        RESULT_VARIABLE result)
    if(NOT result EQUAL 0)
        message(FATAL_ERROR "resgen: invalid definition ${definition}")
    endif()
    include("${out}/entries.cmake")

    add_custom_command(
        OUTPUT "${out}/resources.h"
        COMMAND "${python}" "${RESGEN_SCRIPT}" header "${definition}" "${out}" ${blob_flag}
        DEPENDS "${definition}" "${RESGEN_SCRIPT}"
        COMMENT "resgen: resources.h"
        VERBATIM)
    set(sources "${out}/resources.h")
    set(blobs "")
    foreach(entry ${RESGEN_ENTRIES})
        set(outputs "${out}/${entry}.c")
        if(R_PARTITION)
            list(APPEND outputs "${out}/${entry}.bin")
            list(APPEND blobs "${out}/${entry}.bin")
        endif()
        add_custom_command(
            OUTPUT ${outputs}
            COMMAND "${python}" "${RESGEN_SCRIPT}" entry "${definition}" "${entry}" "${out}" ${blob_flag}
            DEPENDS "${definition}" "${RESGEN_SCRIPT}" ${RESGEN_INPUTS_${entry}}
            COMMENT "resgen: ${entry}"
            VERBATIM)
        list(APPEND sources "${out}/${entry}.c")
    endforeach()

    if(R_PARTITION)
        set(max_size "")
        if(ESP_PLATFORM)
            partition_table_get_partition_info(size "--partition-name ${R_PARTITION}" "size")
            if(NOT size)
                message(FATAL_ERROR "resgen: no partition '${R_PARTITION}' in the partition table")
            endif()
            set(max_size --max-size ${size})
        endif()
        add_custom_command(
            OUTPUT "${out}/resources.c" "${out}/resources.bin"
            COMMAND "${python}" "${RESGEN_SCRIPT}" blob "${definition}" "${out}" "${R_PARTITION}" ${max_size}
            DEPENDS "${definition}" "${RESGEN_SCRIPT}" ${blobs}
            COMMENT "resgen: resources.bin"
            VERBATIM)
        list(APPEND sources "${out}/resources.c")
    endif()

    # The simulator shim adds sources to a target defined in another directory,
    # where custom command outputs are neither GENERATED nor attached to a rule.
    add_custom_target(resgen_${R_TARGET} DEPENDS ${sources})
    add_dependencies(${R_TARGET} resgen_${R_TARGET})
    set_source_files_properties(${sources} TARGET_DIRECTORY ${R_TARGET} PROPERTIES GENERATED TRUE)
    target_sources(${R_TARGET} PRIVATE ${sources})
    target_include_directories(${R_TARGET} PUBLIC "${out}")

    if(R_PARTITION)
        target_sources(${R_TARGET} PRIVATE "${RESGEN_DIR}/resgen_blob.c")
        target_include_directories(${R_TARGET} PRIVATE "${RESGEN_DIR}")
        if(ESP_PLATFORM)
            idf_component_get_property(partition_lib esp_partition COMPONENT_LIB)
            target_link_libraries(${R_TARGET} PRIVATE ${partition_lib})
            esp_partition_register_target(${R_PARTITION} "${out}/resources.bin"
                DEPENDS resgen_${R_TARGET} FLASH_IN_PROJECT)
        endif()
    endif()
endfunction()
